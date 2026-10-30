//! burrow-dispatch, as modules/dispatch.nix runs it: a pass with no
//! arguments, or `status`. DISPATCH_CONFIG names its config.

use std::fs::File;
use std::process::ExitCode;

use burrow_dispatch::config::Config;
use burrow_dispatch::pass::{run, status};
use burrow_dispatch::world::Real;
use burrow_watchdog::now;

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
    match args {
        [] => {
            // One at a time: each rewrites the state.
            let path = cfg.state_dir.join("pass.lock");
            let lock = File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            lock.lock()
                .map_err(|e| format!("{}: {e}", path.display()))?;
            run(&cfg, &Real(&cfg), now())
        }
        [s] if s == "status" => {
            println!("{}", status(&cfg, &Real(&cfg), now()));
            Ok(())
        }
        _ => Err("usage: burrow-dispatch [status]".into()),
    }
}
