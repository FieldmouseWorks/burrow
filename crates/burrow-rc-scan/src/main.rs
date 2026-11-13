//! burrow-rc-scan CONFIG [done | again ID]: claude-rc-scan (scan.rs), as
//! modules/remote-control.nix builds it in with its config.
//!
//! It exits 1 when a scan couldn't finish, and 2 on a usage or a config it
//! can't take.

use std::path::Path;
use std::process::ExitCode;

use burrow_rc_scan::scan::{self, Env};
use burrow_rc_scan::{Broke, Config};

const USAGE: &str = "usage: claude-rc-scan [done | again ID]";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((config, words)) = args.split_first() else {
        eprintln!("usage: burrow-rc-scan CONFIG [done | again ID]");
        return ExitCode::from(2);
    };
    let cfg = match Config::read(Path::new(config)) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("claude-rc-scan: {e}");
            return ExitCode::from(2);
        }
    };
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    let run =
        |go: &dyn Fn(&Env) -> Result<(), Broke>| match Env::from_env().and_then(|env| go(&env)) {
            Ok(()) => ExitCode::SUCCESS,
            Err(Broke(why)) => {
                eprintln!("claude-rc-scan: {why}");
                ExitCode::FAILURE
            }
        };
    match words.as_slice() {
        [] => run(&|env| scan::scan(&cfg, env)),
        ["done"] => run(&|env| Err(scan::done(&cfg, env))),
        ["again", id] => run(&|env| scan::again(&cfg, env, id)),
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}
