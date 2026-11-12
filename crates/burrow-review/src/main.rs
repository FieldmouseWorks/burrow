//! burrow-review, as modules/autoland.nix runs it: `work`, the reviewers'
//! service; `check`, its config read and nothing done; and `bridge SOCKET
//! NAMES -- ARGV...`, what a look's sandbox runs first. REVIEW_CONFIG names
//! the config, and REVIEW_DEPLOYED the generation deployed now (config.rs
//! `Deployed`).

use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use burrow_review::config::Config;
use burrow_review::{Ended, Service, egress, work};

const USAGE: &str = "usage: burrow-review work | check | bridge SOCKET NAME,NAME... -- ARGV...";

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    match command(&args) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("burrow-review: {e}");
            ExitCode::FAILURE
        }
    }
}

fn command(args: &[OsString]) -> Result<u8, String> {
    let first = args.first().and_then(|a| a.to_str());
    match first {
        Some("bridge") => {
            let [_, socket, names, dashes, argv @ ..] = args else {
                return Err(USAGE.into());
            };
            if dashes != "--" {
                return Err(USAGE.into());
            }
            let names = names.to_str().ok_or(USAGE)?;
            let names: Vec<&str> = names.split(',').filter(|n| !n.is_empty()).collect();
            let code = egress::bridge(&PathBuf::from(socket), &names, argv)?;
            Ok(u8::try_from(code & 0xff).unwrap_or(1))
        }
        Some("check") if args.len() == 1 => {
            let cfg = Config::load()?;
            println!(
                "burrow-review: Codex, {} at a time, {} minutes a look; queue {}",
                cfg.reviewers.codex.parallel,
                cfg.timeout_minutes,
                cfg.queue.display()
            );
            Ok(0)
        }
        Some("work") if args.len() == 1 => {
            let cfg = Config::load()?;
            let deployed = cfg.deployed.clone();
            let exe = std::env::current_exe().map_err(|e| format!("can't find itself: {e}"))?;
            let home = std::env::var_os("HOME").ok_or("HOME isn't set")?;
            let s = Service::new(
                cfg,
                exe,
                PathBuf::from(home),
                Arc::new(egress::dial),
                Arc::new(|line| println!("{line}")),
            );
            if work(Arc::new(s)) == Ended::Moved
                && let Some(d) = deployed
            {
                let next = d.program();
                if next.exists() {
                    // The deployed program and its own config, one
                    // generation's, whatever unit systemd started: its
                    // wrapper names its config.
                    let e = Command::new(&next)
                        .arg("work")
                        .env_remove("REVIEW_CONFIG")
                        .exec();
                    return Err(format!("couldn't start {}: {e}", next.display()));
                }
                // A generation without one (from before it had one): its
                // own unit, once systemd has it, starts its run. Not at
                // once, so a start before then can't spend the unit's
                // start limit.
                println!("the deployed generation has no {}", next.display());
                thread::sleep(Duration::from_secs(5));
            }
            Ok(0)
        }
        _ => Err(USAGE.into()),
    }
}
