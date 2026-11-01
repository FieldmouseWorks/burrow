//! `burrow-heavy CONFIG ARGS...`: modules/cpu.nix wraps it as `heavy`, with
//! its config, and the rest of the line is heavy's (lib.rs).

use std::ffi::OsString;
use std::io::{self, Write as _};
use std::path::Path;
use std::process::ExitCode;

use burrow_heavy::{Config, USAGE};

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let Some(config) = args.next() else {
        let _ = writeln!(io::stderr().lock(), "usage: burrow-heavy CONFIG ARGS...");
        return ExitCode::from(USAGE);
    };
    let config = match Config::load(Path::new(&config)) {
        Ok(c) => c,
        Err(e) => {
            let _ = writeln!(io::stderr().lock(), "heavy: {e}");
            return ExitCode::from(1);
        }
    };
    let rest: Vec<OsString> = args.collect();
    ExitCode::from(burrow_heavy::main(&config, &rest))
}
