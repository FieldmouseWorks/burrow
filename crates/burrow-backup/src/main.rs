//! `burrow-backup CONFIG [status | drill [PATH...] | restic ARGS...]`, as
//! HOST-backup (lib.rs). Everything it does needs root, for its state, the
//! password and the snapshots, so started as anyone else it starts again
//! through sudo.

use std::env;
use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use burrow_backup::{Config, Run, compare, drill, exit_code, pass, snapshot, status};

enum Ask {
    Pass,
    Status,
    Drill(Vec<String>),
    Restic(Vec<OsString>),
    /// What it runs itself as, in a snapshot's namespace.
    InSnapshot(Vec<OsString>),
    Compare(PathBuf, Vec<OsString>),
}

fn ask(words: &[OsString], cfg: &Config) -> Option<Ask> {
    let (first, rest) = match words.split_first() {
        None => return Some(Ask::Pass),
        Some((w, rest)) => (w.to_str()?, rest),
    };
    Some(match first {
        "run" if rest.is_empty() => Ask::Pass,
        "status" if rest.is_empty() => Ask::Status,
        "drill" if rest.is_empty() => Ask::Drill(cfg.drill.clone()),
        "drill" => Ask::Drill(
            rest.iter()
                .map(|p| p.to_str().map(str::to_owned))
                .collect::<Option<_>>()?,
        ),
        "restic" => Ask::Restic(rest.to_vec()),
        "__in-snapshot" => Ask::InSnapshot(rest.to_vec()),
        "__compare" => {
            let (base, paths) = rest.split_first()?;
            Ask::Compare(PathBuf::from(base), paths.to_vec())
        }
        _ => return None,
    })
}

fn main() -> ExitCode {
    let args: Vec<OsString> = env::args_os().skip(1).collect();
    let Some((config, words)) = args.split_first() else {
        eprintln!("usage: burrow-backup CONFIG [status | drill [PATH...] | restic ARGS...]");
        return ExitCode::from(2);
    };
    let cfg = match Config::read(Path::new(config)) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("burrow-backup: {e}");
            return ExitCode::from(2);
        }
    };
    let Some(ask) = ask(words, &cfg) else {
        eprintln!(
            "usage: {} [status | drill [PATH...] | restic ARGS...]",
            cfg.name
        );
        return ExitCode::from(2);
    };
    // Started as anyone but root, it starts again through sudo.
    if !rustix::process::geteuid().is_root() {
        let e = match env::current_exe() {
            Ok(exe) => Command::new(&cfg.sudo)
                .arg("--")
                .arg(exe)
                .args(&args)
                .exec(),
            Err(e) => e,
        };
        eprintln!("{}: {}: {e}", cfg.name, cfg.sudo.display());
        return ExitCode::FAILURE;
    }
    let run = match Run::new(&cfg, Path::new(config)) {
        Ok(run) => run,
        Err(e) => {
            eprintln!("{}: {e}", cfg.name);
            return ExitCode::FAILURE;
        }
    };
    match ask {
        Ask::InSnapshot(args) => {
            eprintln!("{}", snapshot::enter(&run, &args));
            ExitCode::FAILURE
        }
        Ask::Compare(base, paths) => match compare::compare(&base, Path::new(""), &paths) {
            Ok(v) => {
                for line in &v.said {
                    println!("{line}");
                }
                if v.ok {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::FAILURE
                }
            }
            Err(e) => {
                eprintln!("{e}");
                ExitCode::FAILURE
            }
        },
        Ask::Pass => {
            if pass::automatic(&run) {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        ask => {
            let _lock = match ready(&run, matches!(ask, Ask::Status)) {
                Ok(Some(lock)) => lock,
                Ok(None) => return ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("{e}");
                    return ExitCode::FAILURE;
                }
            };
            match ask {
                Ask::Status => {
                    status::repository(&run);
                    ExitCode::SUCCESS
                }
                Ask::Drill(paths) => {
                    if drill::drill(&run, &paths) {
                        ExitCode::SUCCESS
                    } else {
                        ExitCode::FAILURE
                    }
                }
                Ask::Restic(args) => {
                    match run.restic().args(&args).stdin(Stdio::inherit()).status() {
                        Ok(s) => ExitCode::from(exit_code(s).clamp(0, 255) as u8),
                        Err(e) => {
                            eprintln!("restic: {e}");
                            ExitCode::FAILURE
                        }
                    }
                }
                _ => unreachable!("the pass and the namespace's own are above"),
            }
        }
    }
}

/// Its state private, the lock held and rclone's copy current. A status
/// says what the state does first, and doesn't wait: None, when a backup or
/// drill holds the lock.
fn ready(run: &Run, status: bool) -> Result<Option<std::fs::File>, String> {
    let cfg = run.cfg;
    burrow_backup::private_state(cfg)?;
    if status {
        print!("{}", status::local(run)?);
    }
    let path = burrow_backup::lock_path(cfg);
    let lock = if status {
        let (lock, free) = burrow_backup::lock(&path)?;
        if !free {
            println!("a backup or drill is running (journalctl -u {})", cfg.name);
            return Ok(None);
        }
        lock
    } else {
        burrow_backup::hold(&path)?
    };
    burrow_backup::rclone(cfg)?;
    Ok(Some(lock))
}
