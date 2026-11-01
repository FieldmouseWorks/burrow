//! `burrow-robot CONFIG [ARGS…]`, which is `robot` (lib.rs) once
//! modules/robot.nix builds the config in.

use std::ffi::OsStr;
use std::io::Write as _;
use std::path::Path;
use std::process::ExitCode;

use burrow_robot::{Config, Env, Robot, agent, credentials, deliver, parse};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((config, rest)) = args.split_first() else {
        say("usage: burrow-robot CONFIG [ARGS...]");
        return ExitCode::from(2);
    };
    let cfg = match Config::read(Path::new(config)) {
        Ok(cfg) => cfg,
        Err(e) => {
            say(format_args!("robot: {e}"));
            return ExitCode::from(2);
        }
    };
    let call = match parse(rest) {
        Ok(call) => call,
        Err(usage) => {
            say(usage);
            return ExitCode::from(2);
        }
    };
    let env = Env::get();
    let creds = match credentials(
        &env,
        &cfg,
        |p| std::fs::read_to_string(p),
        OsStr::new("sudo"),
    ) {
        Ok(creds) => creds,
        Err(e) => {
            say(format_args!("robot: {e}"));
            return ExitCode::FAILURE;
        }
    };
    let robot = Robot::new(agent(), &env, &cfg, &creds);
    let (mut out, mut note) = (String::new(), String::new());
    let ran = robot.run(&call, &mut out, &mut note);
    if deliver(
        ran,
        &out,
        &note,
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    ) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// One line for stderr. eprintln! panics when stderr can't be written, which
/// would trade robot's exit code for a panic's; the exit code says it instead.
fn say(line: impl std::fmt::Display) {
    let _ = writeln!(std::io::stderr(), "{line}");
}
