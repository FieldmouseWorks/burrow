//! Conary `conary-plugin v5` executable for Antigravity: reads one JSON
//! request from stdin, writes one JSON response to stdout, and logs failures on
//! stderr.

use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use burrow_engine_antigravity::{Config, answer};
use conary_core::plugin::Request;

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let config_path = match args.as_slice() {
        [] => None,
        [path] => Some(PathBuf::from(path)),
        _ => {
            return Err(
                "usage: burrow-engine-antigravity [CONFIG_JSON] (speaks conary-plugin v5 on stdio)"
                    .to_owned(),
            );
        }
    };
    let config = Config::load(config_path.as_deref())?;
    let mut input = Vec::new();
    io::stdin()
        .lock()
        .read_to_end(&mut input)
        .map_err(|e| format!("failed to read plugin request: {e}"))?;
    let request: Request = serde_json::from_slice(&input)
        .map_err(|e| format!("failed to decode plugin request: {e}"))?;
    // The turn's instructions are read here, at the process's edge, alone.
    let instructions = burrow_grants::turn::instructions(|name| std::env::var(name).ok());
    let response = answer(&config, &request, instructions.as_deref())?;
    let bytes = serde_json::to_vec(&response)
        .map_err(|e| format!("failed to encode plugin response: {e}"))?;
    io::stdout()
        .write_all(&bytes)
        .map_err(|e| format!("failed to write plugin response: {e}"))?;
    io::stdout()
        .write_all(b"\n")
        .map_err(|e| format!("failed to flush plugin response: {e}"))
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("burrow-engine-antigravity: {why}");
            ExitCode::FAILURE
        }
    }
}
