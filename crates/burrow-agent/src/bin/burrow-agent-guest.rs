//! What burrow-agent runs inside an engine plugin's VM (plugin/guest.rs).
//!
//! ```text
//! burrow-agent-guest plugin PROGRAM UID GID [CREDENTIAL_ENV]
//! burrow-agent-guest heavy [--] COMMAND...
//! ```

use std::ffi::OsString;
use std::process::ExitCode;

use burrow_agent::plugin::guest;

fn run(args: &[OsString]) -> Result<u8, String> {
    let usage = || "usage: burrow-agent-guest plugin PROGRAM UID GID [CREDENTIAL_ENV] | heavy [--] COMMAND...";
    match args {
        [word, rest @ ..] if word == "heavy" => guest::heavy(rest),
        [word, program, uid, gid, env @ ..] if word == "plugin" && env.len() <= 1 => {
            let id = |v: &OsString| v.to_str().and_then(|v| v.parse().ok()).ok_or_else(usage);
            guest::plugin(&guest::Image {
                program: program.into(),
                uid: id(uid)?,
                gid: id(gid)?,
                credential_env: env.first().and_then(|e| e.to_str()).map(str::to_owned),
            })?;
            Ok(0)
        }
        _ => Err(usage().to_owned()),
    }
}

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    match run(&args) {
        Ok(code) => ExitCode::from(code),
        Err(why) => {
            eprintln!("burrow-agent-guest: {why}");
            ExitCode::FAILURE
        }
    }
}
