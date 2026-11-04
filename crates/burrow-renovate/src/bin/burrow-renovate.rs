//! burrow-renovate CONFIG [OWNER/REPO]: renovate-run (run.rs), as
//! modules/updates.nix builds it in with its config.
//!
//!   renovate-run              every repo (burrow-events' catch-up: new
//!                             releases upstream send no event)
//!   renovate-run OWNER/REPO   that repo, if a full run covers it: any repo's
//!                             events reach this unit
//!
//! It exits as Renovate's run did, 1 when something it needed broke, and 2
//! on a usage or a config it can't take.

use std::env;
use std::path::Path;
use std::process::ExitCode;

use burrow_renovate::{run::Config, words};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let Some((config, rest)) = args.split_first() else {
        eprintln!("usage: burrow-renovate CONFIG [OWNER/REPO]");
        return ExitCode::from(2);
    };
    let cfg = match Config::read(Path::new(config)) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("burrow-renovate: {e}");
            return ExitCode::from(2);
        }
    };
    let Some(only) = words(rest) else {
        eprintln!("usage: {} [OWNER/REPO]", cfg.name);
        return ExitCode::from(2);
    };
    ExitCode::from(cfg.run(only))
}
