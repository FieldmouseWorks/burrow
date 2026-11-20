//! burrow-units, as modules/watchdog/units.nix runs it, UNITS_CONFIG
//! naming its config:
//!
//! ```text
//! burrow-units pass        read what's failed and crashed, rewrite the
//!                          watch file, and kick the watchdog when what's in
//!                          it changed (the service, burrow-units.service)
//! burrow-units watch       follow the journal, asking for a pass each time a
//!                          unit fails, one in the watch file recovers or a
//!                          program dumps core
//!                          (burrow-units-watch.service)
//! burrow-units             ask for a pass
//! burrow-units clear EXE.. mark those programs' crashes so far dealt with
//!                          through the host socket, then ask for a pass
//! burrow-units serve-clear handle one socket-activated host request
//! ```
//!
//! Only the service passes, so every pass reads the whole journal, whoever
//! asked for it, and passes never overlap: each holds the lock beside the
//! watch file from what it reads to what it writes, as `clear` does for
//! its own write.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use std::{env, fs, thread};

use serde::Deserialize;

mod clear_door;

use burrow_units::{Found, cores, failed_names, items, shown, unit_key};
use burrow_watchdog::now;
use burrow_watchdog::watch::{File as Watch, Item, read};

/// systemd's own (pid 1's) word that a service or socket failed
/// (UNIT_FAILURE_RESULT) or a start job did (UNIT_FAILED).
const FAILED: [&str; 2] = [
    "d9b373ed55a64feb8242e02dbe79a49c",
    "be02cf6855d2428ba40df7e9d022f03d",
];
/// systemd's own word that a unit started (UNIT_STARTED) or finished well
/// (UNIT_SUCCESS): a recovery, but only of a unit the watch file has. Any
/// other is a timer's service doing its job, which clears nothing. Starting
/// alone (UNIT_STARTING) is no recovery: repeated failed starts must keep
/// the same stall.
const RECOVERED: [&str; 2] = [
    "39f53479d3a045ac8e11786248231fbf",
    "7ad2d189f7e94e70a38c781354912448",
];
/// systemd-coredump's word that a program dumped core.
const DUMPED: &str = "fc2e22bc6ee647b6b90729ab34a250b1";

/// The journalctl matches for the entries the watcher reads: pid 1's
/// FAILED and RECOVERED, or a DUMPED. A user's manager says the same of its
/// own units, which aren't the machine's.
fn follow() -> Vec<String> {
    FAILED
        .iter()
        .chain(&RECOVERED)
        .map(|id| format!("MESSAGE_ID={id}"))
        .chain(["_PID=1".into(), "+".into(), format!("MESSAGE_ID={DUMPED}")])
        .collect()
}
/// Units whose state changes start no pass: its own, since a pass's
/// completion would otherwise ask for another for ever. The hourly
/// catch-up still sees them failed.
const OWN: [&str; 2] = ["burrow-units.service", "burrow-units-watch.service"];
/// How long a burst of entries (a unit failing takes its dependents with
/// it) is gathered into one ask.
const SETTLE: Duration = Duration::from_secs(2);

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Config {
    /// The host's acknowledgement socket. Callers never need its state directory.
    clear_socket: PathBuf,
    /// The watch file the watchdog reads.
    file: PathBuf,
    /// Globs of units and programs never watched.
    ignore: Vec<String>,
    /// How long after it's seen an item falls due, in seconds.
    grace: f64,
    /// How far back a crash counts, in seconds.
    window: f64,
    systemctl: PathBuf,
    coredumpctl: PathBuf,
    journalctl: PathBuf,
    /// Where to ask for a pass: burrow-units' own burrow-events handler.
    own: String,
    /// Where to kick the watchdog's pass, or null for none.
    kick: Option<String>,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("burrow-units: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let path = env::var_os("UNITS_CONFIG").ok_or("UNITS_CONFIG isn't set")?;
    let text = fs::read(&path).map_err(|e| format!("{}: {e}", path.to_string_lossy()))?;
    let cfg: Config =
        serde_json::from_slice(&text).map_err(|e| format!("{}: {e}", path.to_string_lossy()))?;
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("pass") => pass(&cfg),
        Some("watch") => watch(&cfg),
        Some("serve-clear") => clear_door::serve(&cfg),
        None => ask(&cfg),
        Some("clear") => {
            let exes: Vec<String> = args.collect();
            clear_door::validate(&exes)?;
            clear_door::client(&cfg.clear_socket, &exes)
        }
        Some(other) => Err(format!(
            "unknown command {other}: it takes pass, watch, clear EXE..., or none"
        )),
    }
}

/// The lock beside the watch file, held until it's dropped.
fn hold(cfg: &Config) -> Result<File, String> {
    let lock = cfg.file.with_file_name("lock");
    File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock)
        .and_then(|f| f.lock().map(|()| f))
        .map_err(|e| format!("{}: {e}", lock.display()))
}

/// When each program's crashes were last cleared, those longer ago than
/// the window (whose crashes are gone anyway) left out; and where it's kept.
fn cleared(cfg: &Config, now: f64) -> Result<(PathBuf, BTreeMap<String, f64>), String> {
    let at = cfg.file.with_file_name("cleared.json");
    let mut cleared: BTreeMap<String, f64> = match fs::read(&at) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", at.display()))?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
        Err(e) => return Err(format!("{}: {e}", at.display())),
    };
    cleared.retain(|_, t| *t > now - cfg.window);
    Ok((at, cleared))
}

/// EXES' crashes so far marked dealt with.
fn clear(cfg: &Config, exes: &[String]) -> Result<(), String> {
    let _held = hold(cfg)?;
    let now = now();
    let (at, mut cleared) = cleared(cfg, now)?;
    for exe in exes {
        cleared.insert(exe.clone(), now);
    }
    write(
        &at,
        &serde_json::to_vec(&cleared).map_err(|e| e.to_string())?,
    )
}

/// A pass asked of the service.
fn ask(cfg: &Config) -> Result<(), String> {
    request_pass(cfg)?;
    println!("asked for a pass: the watch file has it in a moment");
    Ok(())
}

fn request_pass(cfg: &Config) -> Result<(), String> {
    burrow_events::kick::kick(&cfg.own, Duration::from_secs(10))
        .map_err(|e| format!("couldn't ask for a pass: {e}"))?;
    Ok(())
}

/// Follow the journal for good, asking for a pass after each burst of
/// entries that could change what's failed or crashed, and once as it
/// starts, for what came while it wasn't following. It ends only when
/// journalctl does, which its service restarts.
fn watch(cfg: &Config) -> Result<(), String> {
    let mut child = Command::new(&cfg.journalctl)
        .args(["--follow", "--lines=0", "--quiet", "--output=json"])
        .args(follow())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{}: {e}", cfg.journalctl.display()))?;
    let stdout = child.stdout.take().ok_or("journalctl gave no stdout")?;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if let Some(want) = wants(&line)
                && tx.send(want).is_err()
            {
                break;
            }
        }
    });
    if let Err(e) = ask(cfg) {
        eprintln!("burrow-units: {e}");
    }
    while let Ok(first) = rx.recv() {
        let mut burst = vec![first];
        burst.extend(gather(&rx, SETTLE));
        if needs_a_pass(&burst, || watched(cfg))
            && let Err(e) = ask(cfg)
        {
            eprintln!("burrow-units: {e}");
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    Err(format!("journalctl stopped following: {status}"))
}

/// The rest of a burst whose first entry just came: whatever else RX has
/// by WITHIN from now, however busy the journal stays, so a stream of
/// entries holds an ask back no longer than that.
fn gather<T>(rx: &mpsc::Receiver<T>, within: Duration) -> Vec<T> {
    let by = Instant::now() + within;
    let mut rest = Vec::new();
    while let Some(left) = by.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(got) => rest.push(got),
            Err(_) => break,
        }
    }
    rest
}

/// What a journal entry asks of the watcher.
#[derive(Debug, PartialEq)]
enum Want {
    /// A pass: something may have failed or crashed.
    Pass,
    /// A pass if the watch file has this unit: it may have recovered.
    IfWatched(String),
}

/// What a journal entry (journalctl's JSON) asks for: nothing when it's
/// about burrow-units' own units, the unit that changed (`UNIT`) or the one
/// a program that dumped core ran in (`COREDUMP_UNIT`, a pass's own tools
/// included); a pass if its unit is watched when it's a RECOVERED; a pass
/// otherwise, and when it can't be read.
fn wants(entry: &str) -> Option<Want> {
    let Ok(entry) = serde_json::from_str::<serde_json::Value>(entry) else {
        return Some(Want::Pass);
    };
    let field = |name| entry.get(name).and_then(serde_json::Value::as_str);
    if ["UNIT", "COREDUMP_UNIT"]
        .into_iter()
        .any(|name| field(name).is_some_and(|u| OWN.contains(&u)))
    {
        return None;
    }
    match (field("MESSAGE_ID"), field("UNIT")) {
        (Some(id), Some(unit)) if RECOVERED.contains(&id) => Some(Want::IfWatched(unit.to_owned())),
        _ => Some(Want::Pass),
    }
}

/// Whether a burst of entries asks for a pass: any that wants one outright
/// does, and a recovery does when its unit is in the watch file (WATCHED)
/// or the watch file can't be read, so a recovery is never missed.
fn needs_a_pass(
    burst: &[Want],
    watched: impl FnOnce() -> Result<BTreeMap<String, Item>, String>,
) -> bool {
    let mut recovered = Vec::new();
    for want in burst {
        match want {
            Want::Pass => return true,
            Want::IfWatched(unit) => recovered.push(unit),
        }
    }
    if recovered.is_empty() {
        return false;
    }
    match watched() {
        Ok(items) => recovered.iter().any(|u| items.contains_key(&unit_key(u))),
        Err(e) => {
            eprintln!("burrow-units: {e}, so asking for a pass");
            true
        }
    }
}

/// The watch file's items, read under the lock a pass holds from what it
/// reads to what it writes: a pass that read a unit failed before it
/// recovered has written it by now, and any later pass reads it recovered.
/// The watcher can't write there, so it opens the lock as it is and shares
/// it.
fn watched(cfg: &Config) -> Result<BTreeMap<String, Item>, String> {
    let lock = cfg.file.with_file_name("lock");
    let _held = File::open(&lock)
        .and_then(|f| f.lock_shared().map(|()| f))
        .map_err(|e| format!("{}: {e}", lock.display()))?;
    read(&cfg.file)
        .map(|(_, items)| items)
        .ok_or_else(|| format!("{}: no watch file to read", cfg.file.display()))
}

/// Where the system journal is kept, on disk and before it's flushed there.
const JOURNALS: [&str; 2] = ["/var/log/journal", "/run/log/journal"];

/// Whether this process can read the system journal under ROOTS: one that
/// can't sees only its own user's crashes, and a pass from it would drop
/// everyone else's.
fn sees_the_journal(roots: &[&Path]) -> Result<(), String> {
    let mut denied = None;
    for root in roots {
        let Ok(machines) = fs::read_dir(root) else {
            continue;
        };
        for machine in machines.flatten() {
            let system = machine.path().join("system.journal");
            match File::open(&system) {
                Ok(_) => return Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => denied = Some(format!("{}: {e}", system.display())),
            }
        }
    }
    Err(format!(
        "can't read the system journal ({}), so a pass would miss other users' crashes: \
         run `burrow-units` to ask the service for one",
        denied.unwrap_or_else(|| "none found".into())
    ))
}

/// One pass: what's failed and what's crashed, into the watch file.
fn pass(cfg: &Config) -> Result<(), String> {
    sees_the_journal(&JOURNALS.map(Path::new))?;
    let _held = hold(cfg)?;
    let now = now();
    let (_, cleared) = cleared(cfg, now)?;

    let names = failed_names(&output(
        &cfg.systemctl,
        &[
            "list-units",
            "--state=failed",
            "--plain",
            "--no-legend",
            "--full",
            "--no-pager",
        ],
    )?);
    let failed = if names.is_empty() {
        Vec::new()
    } else {
        let mut args = vec![
            "show",
            "--timestamp=unix",
            "-p",
            "Id,Result,StateChangeTimestamp",
            "--",
        ];
        args.extend(names.iter().map(String::as_str));
        shown(&output(&cfg.systemctl, &args)?, now)
    };
    // coredumpctl fails when it finds nothing, and says so: anything else
    // it says, failing, is a failed pass, which keeps the last watch file
    // (and goes stale, if it lasts) rather than drop a crash still there.
    let since = format!("--since=@{}", (now - cfg.window).floor());
    let dumps = Command::new(&cfg.coredumpctl)
        .args(["list", "--json=short", "--no-pager", &since])
        .output()
        .map_err(|e| format!("{}: {e}", cfg.coredumpctl.display()))?;
    let said = String::from_utf8_lossy(&dumps.stderr);
    let cores = if dumps.status.success() {
        cores(&String::from_utf8_lossy(&dumps.stdout))?
    } else if said.contains("No coredumps found") {
        Vec::new()
    } else {
        return Err(format!(
            "{} list: {}",
            cfg.coredumpctl.display(),
            said.trim()
        ));
    };

    let last = read(&cfg.file).map(|(_, items)| items).unwrap_or_default();
    let found = Found {
        failed: &failed,
        cores: &cores,
        cleared: &cleared,
    };
    let items = items(&found, &cfg.ignore, cfg.grace, &last);
    let changed = items != last;
    let watch = serde_json::to_vec(&Watch { at: now, items }).map_err(|e| e.to_string())?;
    write(&cfg.file, &watch)?;
    if changed
        && let Some(url) = &cfg.kick
        && let Err(e) = burrow_events::kick::kick(url, Duration::from_secs(10))
    {
        eprintln!("burrow-units: couldn't kick the watchdog: {e}");
    }
    Ok(())
}

/// What PROGRAM ARGS prints, or why it failed.
fn output(program: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("{}: {e}", program.display()))?;
    if !out.status.success() {
        return Err(format!(
            "{} {}: {}",
            program.display(),
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// FILE, whole: the watchdog never reads half of one.
fn write(file: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = file.with_extension("tmp");
    fs::write(&tmp, bytes)
        .and_then(|()| fs::rename(&tmp, file))
        .map_err(|e| format!("{}: {e}", file.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pass_needs_the_system_journal() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        assert!(sees_the_journal(&[root]).is_err());
        let machine = root.join("0123456789abcdef");
        fs::create_dir(&machine).unwrap();
        fs::write(machine.join("user-1000.journal"), "").unwrap();
        assert!(sees_the_journal(&[root]).is_err());
        fs::write(machine.join("system.journal"), "").unwrap();
        assert!(sees_the_journal(&[Path::new("/nonexistent"), root]).is_ok());
    }

    #[test]
    fn its_own_units_changing_state_start_no_pass() {
        let pass = Some(Want::Pass);
        assert_eq!(
            wants(r#"{"UNIT":"backup.service","MESSAGE_ID":"d9b373ed55a64feb8242e02dbe79a49c"}"#),
            pass
        );
        assert_eq!(wants(r#"{"COREDUMP_EXE":"/bin/bash"}"#), pass);
        assert_eq!(
            wants(r#"{"COREDUMP_EXE":"/bin/bash","COREDUMP_UNIT":"backup.service"}"#),
            pass
        );
        // systemd-coredump itself failing is anyone's to know.
        assert_eq!(
            wants(r#"{"UNIT":"systemd-coredump@0-1-2_3-0.service"}"#),
            pass
        );
        assert_eq!(wants("not json"), pass);
        for own in OWN {
            assert_eq!(wants(&format!(r#"{{"UNIT":"{own}"}}"#)), None, "{own}");
            // Watching successful completions must not make each pass
            // schedule another one when its own service finishes.
            for id in RECOVERED {
                let completed = format!(r#"{{"UNIT":"{own}","MESSAGE_ID":"{id}"}}"#);
                assert_eq!(wants(&completed), None, "{completed}");
            }
            let dumped =
                format!(r#"{{"COREDUMP_EXE":"/bin/coredumpctl","COREDUMP_UNIT":"{own}"}}"#);
            assert_eq!(wants(&dumped), None, "{own}");
        }
    }

    #[test]
    fn a_unit_starting_or_succeeding_asks_only_if_it_is_watched() {
        for id in RECOVERED {
            let entry = format!(r#"{{"UNIT":"reconcile.service","MESSAGE_ID":"{id}"}}"#);
            assert_eq!(
                wants(&entry),
                Some(Want::IfWatched("reconcile.service".into()))
            );
            // One with no unit to look up can't be told apart: a pass.
            assert_eq!(
                wants(&format!(r#"{{"MESSAGE_ID":"{id}"}}"#)),
                Some(Want::Pass)
            );
        }
        let watching = |units: &[&str]| -> Result<BTreeMap<String, Item>, String> {
            Ok(units
                .iter()
                .map(|u| (unit_key(u), Item::default()))
                .collect())
        };
        let recovered = |u: &str| Want::IfWatched(u.into());
        // A timer's service doing its job, every 30s: nothing to clear.
        assert!(!needs_a_pass(&[recovered("reconcile.service")], || {
            watching(&["backup.service"])
        }));
        assert!(!needs_a_pass(&[recovered("reconcile.service")], || {
            watching(&[])
        }));
        // The failed unit running again leaves the watch file.
        assert!(needs_a_pass(
            &[recovered("reconcile.service"), recovered("backup.service")],
            || watching(&["backup.service"])
        ));
        // A watch file that can't be read can't say: a pass.
        assert!(needs_a_pass(&[recovered("reconcile.service")], || Err(
            "unreadable".into()
        )));
        // Anything else in the burst wants one anyway, without a look.
        assert!(needs_a_pass(
            &[recovered("reconcile.service"), Want::Pass],
            || -> Result<_, String> { panic!("no need to look") }
        ));
        assert!(!needs_a_pass(&[], || -> Result<_, String> {
            panic!("no need to look")
        }));
    }

    #[test]
    fn the_watcher_reads_the_watch_file_under_the_lock_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config {
            clear_socket: dir.path().join("clear.sock"),
            file: dir.path().join("watch.json"),
            ignore: Vec::new(),
            grace: 600.0,
            window: 3600.0,
            systemctl: "systemctl".into(),
            coredumpctl: "coredumpctl".into(),
            journalctl: "journalctl".into(),
            own: String::new(),
            kick: None,
        };
        // Before any pass, there's no lock to share, and nothing it makes.
        assert!(watched(&cfg).is_err());
        assert!(!dir.path().join("lock").exists());
        let items = BTreeMap::from([(unit_key("backup.service"), Item::default())]);
        let watch = Watch {
            at: 5.0,
            items: items.clone(),
        };
        write(&cfg.file, &serde_json::to_vec(&watch).unwrap()).unwrap();
        drop(hold(&cfg).unwrap());
        assert_eq!(watched(&cfg), Ok(items));
    }

    #[test]
    fn a_stream_of_entries_holds_an_ask_back_no_longer_than_a_burst() {
        let (tx, rx) = mpsc::channel();
        let feed = thread::spawn(move || {
            for _ in 0..40 {
                if tx.send(()).is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(50));
            }
        });
        let start = Instant::now();
        let rest = gather(&rx, Duration::from_millis(300));
        let took = start.elapsed();
        assert!(took >= Duration::from_millis(300), "{took:?}");
        assert!(took < Duration::from_millis(1500), "{took:?}");
        assert!(!rest.is_empty());
        drop(rx);
        feed.join().unwrap();
    }

    #[test]
    fn the_watch_file_reads_back_as_the_watchdog_reads_it() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("watch.json");
        let items = BTreeMap::from([(
            "unit:backup.service".to_owned(),
            Item {
                what: "The unit backup.service failed".into(),
                phase: "failed (exit-code)".into(),
                since: 100.0,
                due: 700.0,
                ..Item::default()
            },
        )]);
        let watch = Watch {
            at: 5.0,
            items: items.clone(),
        };
        write(&file, &serde_json::to_vec(&watch).unwrap()).unwrap();
        assert_eq!(read(&file), Some((5.0, items)));
        assert!(!dir.path().join("watch.tmp").exists());
    }
}
