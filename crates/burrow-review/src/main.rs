//! burrow-review, as modules/autoland.nix runs it: `work`, the reviewers'
//! service; `check`, its config read and nothing done; and `bridge SOCKET
//! NAMES -- ARGV...`, what a look's sandbox runs first. REVIEW_CONFIG names
//! the config.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use burrow_review::config::Config;
use burrow_review::{Service, egress, work};

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
            let exe = std::env::current_exe().map_err(|e| format!("can't find itself: {e}"))?;
            let home = std::env::var_os("HOME").ok_or("HOME isn't set")?;
            let path = std::env::var_os("PATH").unwrap_or_default();
            let s = Service::new(
                cfg,
                exe,
                PathBuf::from(home),
                path,
                Arc::new(egress::dial),
                Arc::new(|line| println!("{line}")),
            );
            work(Arc::new(s));
            Ok(0)
        }
        _ => Err(USAGE.into()),
    }
}
