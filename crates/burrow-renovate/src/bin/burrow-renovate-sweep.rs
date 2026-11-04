//! burrow-renovate-sweep CONFIG [OWNER/REPO]: renovate-sweep (sweep.rs), as
//! modules/updates.nix builds it in with its config.
//!
//!   renovate-sweep              every repo, and the owner's notices
//!   renovate-sweep OWNER/REPO   that repo's PRs only (burrow-events runs it
//!                               when one of its Renovate PRs opens, moves or
//!                               finishes CI); the notices wait for a full one
//!
//! It exits 1 when something it needed broke, and 2 on a usage or a config
//! it can't take.

use std::env;
use std::path::Path;
use std::process::ExitCode;

use burrow_renovate::{sweep::Config, words};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let Some((config, rest)) = args.split_first() else {
        eprintln!("usage: burrow-renovate-sweep CONFIG [OWNER/REPO]");
        return ExitCode::from(2);
    };
    let cfg = match Config::read(Path::new(config)) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("burrow-renovate-sweep: {e}");
            return ExitCode::from(2);
        }
    };
    let Some(only) = words(rest) else {
        eprintln!("usage: {} [OWNER/REPO]", cfg.name);
        return ExitCode::from(2);
    };
    ExitCode::from(cfg.run(only))
}
