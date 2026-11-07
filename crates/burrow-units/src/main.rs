//! burrow-units, as modules/watchdog/units.nix runs it: one pass, with
//! UNITS_CONFIG naming its config. It reads what's failed and what's
//! crashed, rewrites its watch file, and kicks the watchdog when what's in
//! it changed.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;
use std::{env, fs};

use serde::Deserialize;

use burrow_units::{Found, cores, failed_names, items, shown};
use burrow_watchdog::now;
use burrow_watchdog::watch::{File, read};

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
    let now = now();

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
    // coredumpctl fails when it finds nothing: no dumps is what that says.
    let since = format!("--since=@{}", (now - cfg.window).floor());
    let dumps = Command::new(&cfg.coredumpctl)
        .args(["list", "--json=short", "--no-pager", &since])
        .output()
        .map_err(|e| format!("{}: {e}", cfg.coredumpctl.display()))?;
    let cores = cores(&String::from_utf8_lossy(&dumps.stdout));

    let last = read(&cfg.file).map(|(_, items)| items).unwrap_or_default();
    let found = Found {
        failed: &failed,
        cores: &cores,
    };
    let items = items(&found, &cfg.ignore, cfg.grace, &last);
    let changed = items != last;
    write(&cfg.file, &File { at: now, items })?;
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

/// The watch file, whole: the watchdog never reads half of one.
fn write(file: &Path, watch: &File) -> Result<(), String> {
    let tmp = file.with_extension("tmp");
    let json = serde_json::to_vec(watch).map_err(|e| e.to_string())?;
    fs::write(&tmp, json)
        .and_then(|()| fs::rename(&tmp, file))
        .map_err(|e| format!("{}: {e}", file.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use burrow_watchdog::watch::Item;
    use std::collections::BTreeMap;

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
        write(
            &file,
            &File {
                at: 5.0,
                items: items.clone(),
            },
        )
        .unwrap();
        assert_eq!(read(&file), Some((5.0, items)));
        assert!(!dir.path().join("watch.tmp").exists());
    }
}
