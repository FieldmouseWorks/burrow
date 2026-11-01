//! `burrow-robot CONFIG [ARGS…]`, which is `robot` (lib.rs) once
//! modules/robot.nix builds the config in.

use std::ffi::OsStr;
use std::io::Write as _;
use std::path::Path;
use std::process::ExitCode;

use burrow_robot::{Config, Env, Robot, agent, credentials, parse};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((config, rest)) = args.split_first() else {
        eprintln!("usage: burrow-robot CONFIG [ARGS...]");
        return ExitCode::from(2);
    };
    let cfg = match Config::read(Path::new(config)) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("robot: {e}");
            return ExitCode::from(2);
        }
    };
    let call = match parse(rest) {
        Ok(call) => call,
        Err(usage) => {
            eprintln!("{usage}");
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
            eprintln!("robot: {e}");
            return ExitCode::FAILURE;
        }
    };
    let robot = Robot::new(agent(), &env, &cfg, &creds);
    let (mut out, mut note) = (String::new(), String::new());
    let ran = robot.run(&call, &mut out, &mut note);
    // What it got before a call failed still prints.
    let _ = std::io::stdout().write_all(out.as_bytes());
    let _ = std::io::stderr().write_all(note.as_bytes());
    match ran {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("robot: {e}");
            ExitCode::FAILURE
        }
    }
}
