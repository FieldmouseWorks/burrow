//! `burrow-scratch sweep JOBS` removes what proj set aside in JOBS (lib.rs);
//! `burrow-scratch notice JOBS RECORDS UNIT [NOTIFY]` raises and closes the
//! owner's notice for each entry the sweep couldn't remove. modules/proj.nix
//! runs each from a unit of its own, the sweep confined to JOBS.

use std::path::Path;
use std::process::ExitCode;
use std::thread::sleep;
use std::time::Duration;

/// How long a sweep that leaves a watched name behind waits before it exits.
/// The path unit starts it again at once while one is there, and with no
/// pause that would run into its trigger limit and stop watching for good.
const PAUSE: Duration = Duration::from_secs(60);

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args[..] {
        ["sweep", jobs] => sweep(Path::new(jobs)),
        ["notice", jobs, records, unit] => notice(jobs, records, unit, None),
        ["notice", jobs, records, unit, notify] => notice(jobs, records, unit, Some(notify)),
        _ => {
            eprintln!("usage: burrow-scratch sweep JOBS | notice JOBS RECORDS UNIT [NOTIFY]");
            ExitCode::from(2)
        }
    }
}

fn sweep(jobs: &Path) -> ExitCode {
    let done = match burrow_scratch::sweep(jobs) {
        Ok(done) => done,
        Err(e) => {
            eprintln!("burrow-scratch: {}: {e}", jobs.display());
            sleep(PAUSE);
            return ExitCode::FAILURE;
        }
    };
    for name in &done.removed {
        eprintln!("burrow-scratch: removed {}", name.to_string_lossy());
    }
    for (name, why) in &done.still {
        eprintln!(
            "burrow-scratch: {} is still stuck: {why}",
            name.to_string_lossy()
        );
    }
    for (name, why) in &done.stuck {
        eprintln!(
            "burrow-scratch: set {} aside as stuck: {why}",
            name.to_string_lossy()
        );
    }
    for (name, why) in &done.held {
        eprintln!("burrow-scratch: {} stays: {why}", name.to_string_lossy());
    }
    if !done.held.is_empty() {
        sleep(PAUSE);
    }
    // A stuck entry is the notice's, not a failed unit: the sweep did its job.
    ExitCode::SUCCESS
}

fn notice(jobs: &str, records: &str, unit: &str, notify: Option<&str>) -> ExitCode {
    match burrow_scratch::notice(
        Path::new(jobs),
        Path::new(records),
        unit,
        notify.map(Path::new),
    ) {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("burrow-scratch: notices for {jobs}: {e}");
            ExitCode::FAILURE
        }
    }
}
