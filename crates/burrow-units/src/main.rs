//! burrow-units, as modules/watchdog/units.nix runs it: one pass, with
//! UNITS_CONFIG naming its config. It reads what's failed and what's
//! crashed, rewrites its watch file, and kicks the watchdog when what's in
//! it changed. `burrow-units clear EXE...` first marks those programs'
//! crashes so far as dealt with (their cause fixed), so they leave the
//! watch. One pass at a time, however it was started: each holds the lock
//! beside the watch file from what it reads to what it writes.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;
use std::{env, fs};

use serde::Deserialize;

use burrow_units::{Found, cores, failed_names, items, shown};
use burrow_watchdog::now;
use burrow_watchdog::watch::{File as Watch, read};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Config {
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
    let lock = cfg.file.with_file_name("lock");
    let held = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock)
        .and_then(|f| f.lock().map(|()| f))
        .map_err(|e| format!("{}: {e}", lock.display()))?;
    let now = now();

    // Crashes cleared longer ago than the window are long gone anyway.
    let cleared_at = cfg.file.with_file_name("cleared.json");
    let mut cleared: BTreeMap<String, f64> = match fs::read(&cleared_at) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", cleared_at.display()))?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
        Err(e) => return Err(format!("{}: {e}", cleared_at.display())),
    };
    cleared.retain(|_, at| *at > now - cfg.window);
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        None => {}
        Some("clear") => {
            let exes: Vec<String> = args.collect();
            if exes.is_empty() {
                return Err("clear takes the programs' paths, as the watch names them".into());
            }
            for exe in exes {
                cleared.insert(exe, now);
            }
        }
        Some(other) => {
            return Err(format!(
                "unknown command {other}: it takes none, or clear EXE..."
            ));
        }
    }
    write(
        &cleared_at,
        &serde_json::to_vec(&cleared).map_err(|e| e.to_string())?,
    )?;

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
    drop(held);
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
    use burrow_watchdog::watch::Item;

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
