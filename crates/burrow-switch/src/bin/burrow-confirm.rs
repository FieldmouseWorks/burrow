//! burrow-confirm CONFIG: HOST-confirm (confirm.rs), as modules/switch.nix
//! builds it in with its config. The system HOST-switch put live is kept,
//! and booted into from now on.
//!
//! It needs root, so one started as anyone else starts again through sudo.
//! It exits 1 when there was a switch it couldn't keep, and 2 on a usage or
//! a config it can't take.

use std::env;
use std::path::Path;
use std::process::ExitCode;

use burrow_switch::{Broke, Config, THROUGH_SUDO, as_root, confirm};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let Some((config, words)) = args.split_first() else {
        eprintln!("usage: burrow-confirm CONFIG");
        return ExitCode::from(2);
    };
    let cfg = match Config::read(Path::new(config)) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("burrow-confirm: {e}");
            return ExitCode::from(2);
        }
    };
    let through_sudo = match words {
        [] => false,
        [w] if w == THROUGH_SUDO => true,
        _ => {
            eprintln!("usage: {}", cfg.confirm);
            return ExitCode::from(2);
        }
    };
    if let Some(why) = as_root(&cfg.tools.sudo, config, through_sudo, &[] as &[&str]) {
        eprintln!("{}: {why}", cfg.confirm);
        return ExitCode::FAILURE;
    }
    match confirm::confirm(&cfg) {
        Ok(code) => ExitCode::from(code),
        Err(Broke(why)) => {
            eprintln!("{}: {why}", cfg.confirm);
            ExitCode::FAILURE
        }
    }
}
