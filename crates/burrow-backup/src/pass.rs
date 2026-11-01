//! A pass: back up the newest snapshot, if it isn't off the box yet, then
//! the day's upkeep. What stops one is handed to a Claude job and, once the
//! newest copy off the box is old enough, told to the owner.

use std::ffi::OsString;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::mounts::Mounts;
use crate::{
    Config, Run, age, backup_flags, drill, exit_code, local, now, read, remove, snapshot, stamp,
    tail, upkeep, write, zfs,
};

/// What state/last.json keeps of the newest backup.
#[derive(Debug, Serialize, Deserialize)]
pub struct Last {
    pub time: String,
    pub epoch: i64,
    pub snapshot: String,
    pub id: Value,
    pub files: Value,
    pub bytes: Value,
    pub added: Value,
}

impl Last {
    /// The line a pass and `status` say it in.
    pub fn line(&self) -> String {
        let n = |v: &Value| v.as_f64().unwrap_or(0.0);
        format!(
            "{}: {} files, {} GB read, {} MB sent",
            self.snapshot,
            self.files,
            round2(n(&self.bytes) / 1e9),
            round2(n(&self.added) / 1e6)
        )
    }
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// One pass, which ends with the newest snapshot off the box and the day's
/// upkeep done; or why it didn't.
pub fn pass(run: &Run) -> Result<(), String> {
    let cfg = run.cfg;
    let state = &cfg.state;
    let last = read(&state.join("last"))?.filter(|l| !l.is_empty());
    zfs::release_except(run, last.as_deref())?;
    let Some((snap, created)) = zfs::newest(run, &cfg.snapshots)? else {
        return Err(format!(
            "no snapshot matching {} that all of {} have under one name (for sanoid, that's recursive = \"zfs\")",
            cfg.snapshots,
            cfg.datasets.join(" ")
        ));
    };
    if stale(cfg, created, now()) {
        return Err(format!(
            "the newest snapshot matching {}, {snap}, is more than {} hours old: is whatever takes them still running?",
            cfg.snapshots, cfg.stale_after
        ));
    }
    let _ = run
        .restic()
        .args(["unlock", "-q"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if last.as_deref() != Some(snap.as_str()) {
        back_up(run, last.as_deref(), &snap, created)?;
    }
    upkeep::maintain(run)?;
    if let Some(problems) = read(&state.join("problems"))?.filter(|p| !p.is_empty()) {
        return Err(problems);
    }
    remove(&state.join("failed-since"))?;
    remove(&state.join("job"))?;
    // A no-op unless a notice is open, and retried until GitHub takes the close.
    notify(run, &["close", "backup", &format!("Backed up {snap}.")])
}

fn back_up(run: &Run, last: Option<&str>, snap: &str, created: i64) -> Result<(), String> {
    let cfg = run.cfg;
    let state = &cfg.state;
    let opened = run
        .restic()
        .args(["cat", "config"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .output()
        .map_err(|e| format!("restic: {e}"))?;
    match exit_code(opened.status) {
        0 => {}
        // No repository there yet.
        10 => {
            let made = run
                .restic()
                .args(["init", "-q"])
                .stdin(Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            if !made {
                return Err(format!(
                    "couldn't create the repository at {}",
                    cfg.repository
                ));
            }
        }
        rc => {
            return Err(format!(
                "couldn't open the repository at {} (restic exit {rc}): {}",
                cfg.repository,
                tail(&opened.stderr, 3)
            ));
        }
    }
    // Held while it's read, and after, for the drill.
    zfs::hold(run, snap);
    let let_go = || {
        if let Err(e) = zfs::release_except(run, last) {
            eprintln!("{e}");
        }
    };
    let mounts = Mounts::find(&cfg.datasets).inspect_err(|_| let_go())?;
    let mut cmd: Vec<OsString> = ["restic", "backup", "--json", "--quiet", "--host"]
        .map(OsString::from)
        .into();
    cmd.extend([cfg.host.as_str(), "--tag", snap, "--time", &local(created)].map(OsString::from));
    cmd.extend(backup_flags(cfg));
    cmd.extend(mounts.paths().into_iter().map(OsString::from));
    let out = snapshot::command(run, snap, None, &cmd)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("unshare: {e}"))?;
    let summary = drill::summary(&out.stdout);
    let Some(summary) = summary.filter(|_| out.status.success()) else {
        let_go();
        return Err(format!(
            "restic backup of {snap} exited {}: {}",
            exit_code(out.status),
            errors(&out.stderr)
        ));
    };
    let field = |k: &str| summary.get(k).cloned().unwrap_or(Value::Null);
    let backed = Last {
        time: stamp(now()),
        epoch: now(),
        snapshot: snap.to_owned(),
        id: field("snapshot_id"),
        files: field("total_files_processed"),
        bytes: field("total_bytes_processed"),
        added: field("data_added_packed"),
    };
    let json = serde_json::to_string_pretty(&backed).map_err(|e| e.to_string())? + "\n";
    write(&state.join("last.json"), json.as_bytes())?;
    write(&state.join("last"), format!("{snap}\n").as_bytes())?;
    zfs::release_except(run, Some(snap))?;
    println!("backed up {}", backed.line());
    Ok(())
}

/// What restic said went wrong: its errors, as `--json` gives them, or else
/// the last of what it wrote. Five lines at most.
fn errors(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let parsed: Option<Vec<Value>> = text
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).ok())
        .collect();
    let lines: Vec<String> = match parsed {
        Some(all) => all
            .iter()
            .filter(|v| v.get("message_type").and_then(Value::as_str) == Some("error"))
            .map(|v| {
                let item = v.get("item").and_then(Value::as_str).unwrap_or("");
                let e = v.get("error").cloned().unwrap_or(Value::Null);
                let msg = e.get("message").cloned().unwrap_or(e);
                let msg = msg.as_str().map_or_else(|| msg.to_string(), str::to_owned);
                format!("{item}: {msg}")
            })
            .collect(),
        None => tail(stderr, 5).lines().map(str::to_owned).collect(),
    };
    lines.into_iter().take(5).collect::<Vec<_>>().join("\n")
}

/// Hands a failure of the automatic run to a Claude job in the machine's
/// own project, once while one is still on it (or was in the last 6 hours).
/// The owner hears only once the newest copy off the box is staleAfter hours
/// old (at once, with no proj).
pub fn failed(run: &Run, why: &str) {
    let cfg = run.cfg;
    let state = &cfg.state;
    eprintln!("{why}");
    let now = now();
    let since = state.join("failed-since");
    if !since.exists()
        && let Err(e) = write(&since, format!("{now}\n").as_bytes())
    {
        eprintln!("{e}");
    }
    let job = state.join("job");
    if let Some(proj) = &cfg.proj
        && job_due(&job, SystemTime::now())
    {
        hand_over(run, proj, &task(cfg, why), &job);
    }
    let last = std::fs::read(state.join("last.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Last>(&b).ok())
        .map_or(0, |l| l.epoch);
    if owner_hears(cfg, last, now) {
        let (title, body) = notice(cfg, last, why);
        if let Err(e) = notify(run, &["open", "backup", &title, &body]) {
            eprintln!("{e}");
        }
    }
}

/// How long a job has a failure before another is started for it.
const JOB: Duration = Duration::from_secs(6 * 3600);

/// Whether a failure goes to a new job: none has one, or the last was
/// started 6 hours ago or more.
fn job_due(job: &Path, now: SystemTime) -> bool {
    match std::fs::metadata(job) {
        Ok(m) if m.len() > 0 => m
            .modified()
            .ok()
            .map(|t| age(t, now))
            .is_none_or(|age| age >= JOB),
        _ => true,
    }
}

/// Whether the owner hears of a failure: at once with no proj, and with it
/// once the newest copy off the box (`last`, 0 for none) is staleAfter
/// hours old.
fn owner_hears(cfg: &Config, last: i64, now: i64) -> bool {
    cfg.proj.is_none() || stale(cfg, last, now)
}

/// Whether what's from `then` is staleAfter hours old at `now`.
fn stale(cfg: &Config, then: i64, now: i64) -> bool {
    now - then >= cfg.stale_after as i64 * 3600
}

/// The owner's notice: its title and body.
fn notice(cfg: &Config, last: i64, why: &str) -> (String, String) {
    let from = if last == 0 {
        "never".to_owned()
    } else {
        stamp(last)
    };
    let job = if cfg.proj.is_some() {
        " A Claude job has it (`proj status`)."
    } else {
        ""
    };
    (
        format!("{}'s off-site backup is behind", cfg.host),
        format!(
            "The newest copy off {host} is from {from}. The last run said: {why}\n\n`{name} status` on {host} shows more.{job}",
            host = cfg.host,
            name = cfg.name,
        ),
    )
}

/// What the job is told.
fn task(cfg: &Config, why: &str) -> String {
    format!(
        "{name}, the off-site backup, failed on {host}: {why}\n\n\
         The journal has the rest (journalctl -u {name}). Find the cause and fix it for good: in this flake if it's the machine's config, upstream in burrow (modules/backup.nix, crates/burrow-backup) if it's the module. \
         `{name} status` shows where it stands and `sudo systemctl start {name}` runs it again (remove {state}/maintained first to redo the day's prune, check and drill). \
         Never forget, prune or delete anything in the repository by hand, and never remove or re-create it: it's the only copy off the box. \
         If it needs {owner} (a login only they can renew, say), say exactly what.",
        name = cfg.name,
        host = cfg.host,
        state = cfg.state.display(),
        owner = cfg.owner,
    )
}

fn hand_over(run: &Run, proj: &Path, task: &str, job: &Path) {
    let cfg = run.cfg;
    // runuser sets HOME from the user's entry; proj finds the user's
    // runtime dir itself, so root's isn't passed on.
    let out = run
        .command("runuser")
        .args(["-u", &cfg.user, "--"])
        .arg(proj)
        .args(["run", &cfg.project, task])
        .env_remove("XDG_RUNTIME_DIR")
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let id = String::from_utf8_lossy(&o.stdout).trim_end().to_owned();
            match write(job, format!("{id}\n").as_bytes()) {
                Ok(()) => println!("handed to Claude: proj log {id}"),
                Err(e) => eprintln!("{e}"),
            }
        }
        Ok(o) => eprintln!("proj run failed ({})", o.status),
        Err(e) => eprintln!("runuser: {e}"),
    }
}

fn notify(run: &Run, args: &[&str]) -> Result<(), String> {
    let status = run
        .command(&run.cfg.notify)
        .args(args)
        .stdin(Stdio::null())
        .status()
        .map_err(|e| format!("{}: {e}", run.cfg.notify.display()))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "{} {} failed ({status})",
            run.cfg.notify.display(),
            args[0]
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_goes_to_a_job_once_every_6_hours() {
        let dir = tempfile::tempdir().unwrap();
        let job = dir.path().join("job");
        let now = SystemTime::now();
        assert!(job_due(&job, now), "no job yet");
        std::fs::write(&job, "").unwrap();
        assert!(job_due(&job, now), "an empty one");
        std::fs::write(&job, "42\n").unwrap();
        assert!(!job_due(&job, now), "started a moment after now");
        let started = std::fs::metadata(&job).unwrap().modified().unwrap();
        assert!(!job_due(&job, started + JOB - Duration::from_secs(1)));
        assert!(job_due(&job, started + JOB));
    }

    #[test]
    fn the_owner_hears_once_the_copy_off_the_box_is_stale() {
        let mut cfg = crate::config::for_tests(Path::new("/s"));
        let now = 1_700_000_000;
        assert!(!owner_hears(&cfg, now - 6 * 3600 + 1, now));
        assert!(owner_hears(&cfg, now - 6 * 3600, now));
        assert!(owner_hears(&cfg, 0, now), "never backed up");
        cfg.proj = None;
        assert!(owner_hears(&cfg, now, now), "no job to hand it to");
    }

    #[test]
    fn the_notice_and_the_task_say_what_failed() {
        let mut cfg = crate::config::for_tests(Path::new("/var/lib/burrow/backup"));
        let (title, body) = notice(&cfg, 0, "restic backup of s exited 1: x");
        assert_eq!(title, "box's off-site backup is behind");
        assert_eq!(
            body,
            "The newest copy off box is from never. The last run said: restic backup of s exited 1: x\n\n`box-backup status` on box shows more. A Claude job has it (`proj status`)."
        );
        let (_, body) = notice(&cfg, 1_700_000_000, "why");
        assert!(
            body.starts_with(&format!(
                "The newest copy off box is from {}. ",
                stamp(1_700_000_000)
            )),
            "{body}"
        );
        cfg.proj = None;
        assert!(
            notice(&cfg, 0, "why")
                .1
                .ends_with("`box-backup status` on box shows more.")
        );
        assert_eq!(
            task(&cfg, "why"),
            "box-backup, the off-site backup, failed on box: why\n\nThe journal has the rest (journalctl -u box-backup). Find the cause and fix it for good: in this flake if it's the machine's config, upstream in burrow (modules/backup.nix, crates/burrow-backup) if it's the module. `box-backup status` shows where it stands and `sudo systemctl start box-backup` runs it again (remove /var/lib/burrow/backup/maintained first to redo the day's prune, check and drill). Never forget, prune or delete anything in the repository by hand, and never remove or re-create it: it's the only copy off the box. If it needs Sam (a login only they can renew, say), say exactly what."
        );
    }

    #[test]
    fn a_backup_is_said_in_gb_and_mb() {
        let last: Last = serde_json::from_str(
            r#"{"time":"t","epoch":1,"snapshot":"snap-1","id":"abc","files":12,"bytes":12345678901,"added":50000}"#,
        )
        .unwrap();
        assert_eq!(last.line(), "snap-1: 12 files, 12.35 GB read, 0.05 MB sent");
        let last: Last = serde_json::from_str(
            r#"{"time":"t","epoch":1,"snapshot":"s","id":"abc","files":0,"bytes":2000000000,"added":0}"#,
        )
        .unwrap();
        assert_eq!(last.line(), "s: 0 files, 2 GB read, 0 MB sent");
    }

    #[test]
    fn restics_errors_are_said_as_it_gave_them() {
        let json = br#"{"message_type":"error","error":{"message":"permission denied"},"during":"archival","item":"/data/x"}
{"message_type":"status"}
{"message_type":"error","error":"plain","item":null}
"#;
        assert_eq!(errors(json), "/data/x: permission denied\n: plain");
        assert_eq!(
            errors(b"Fatal: one\ntwo\nthree\nfour\nfive\nsix\n"),
            "two\nthree\nfour\nfive\nsix"
        );
        let many: String = (0..7).map(|i| format!("{{\"message_type\":\"error\",\"error\":{{\"message\":\"m{i}\"}},\"item\":\"i\"}}\n")).collect();
        assert_eq!(errors(many.as_bytes()).lines().count(), 5);
    }
}
