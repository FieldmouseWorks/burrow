//! burrow-watchdog, as modules/watchdog.nix runs it: a pass with no
//! arguments, or `status`, or `snooze SOURCE KEY MINUTES WHY`. WATCHDOG_CONFIG
//! names its config.

use std::fs::File;
use std::process::ExitCode;

use burrow_watchdog::config::Config;
use burrow_watchdog::ladder::{run, snooze, status};
use burrow_watchdog::now;
use burrow_watchdog::world::Real;

const USAGE: &str = "usage: burrow-watchdog [status | snooze SOURCE KEY MINUTES WHY]";

fn main() -> ExitCode {
    match command(&std::env::args().skip(1).collect::<Vec<_>>()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn command(args: &[String]) -> Result<(), String> {
    let cfg = Config::load()?;
    let now = now();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    if args == ["status"] {
        println!("{}", status(&cfg, now));
        return Ok(());
    }
    let snoozing = match args[..] {
        [] => None,
        ["snooze", source, key, minutes, why]
            if !minutes.is_empty() && minutes.bytes().all(|b| b.is_ascii_digit()) =>
        {
            Some((source, key, minutes.parse().map_err(|_| USAGE)?, why))
        }
        _ => return Err(USAGE.into()),
    };
    // One at a time: a pass and a snooze each rewrite the state.
    let path = cfg.state_dir.join("pass.lock");
    let lock = File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    lock.lock()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    match snoozing {
        Some((source, key, minutes, why)) => {
            println!("{}", snooze(&cfg, source, key, minutes, why, now)?);
            Ok(())
        }
        None => run(&cfg, &Real(&cfg), now),
    }
}
