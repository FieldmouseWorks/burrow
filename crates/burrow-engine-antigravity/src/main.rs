//! Conary `conary-plugin v5` executable for Antigravity: reads one JSON
//! request from stdin, writes one JSON response to stdout, and logs failures on
//! stderr.

use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use burrow_engine_antigravity::{Config, plugin};
use conary_core::plugin::Request;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let config_path = match args.as_slice() {
        [] => None,
        [path] => Some(PathBuf::from(path)),
        _ => {
            eprintln!(
                "burrow-engine-antigravity: usage: burrow-engine-antigravity [CONFIG_JSON] (speaks conary-plugin v5 on stdio)"
            );
            return ExitCode::from(plugin::EXIT_FAILURE);
        }
    };
    let config = match Config::load(config_path.as_deref()) {
        Ok(c) => c,
        Err(why) => {
            eprintln!("burrow-engine-antigravity: {why}");
            return ExitCode::from(plugin::EXIT_FAILURE);
        }
    };
    let mut input = Vec::new();
    if let Err(e) = io::stdin().lock().read_to_end(&mut input) {
        eprintln!("burrow-engine-antigravity: failed to read plugin request: {e}");
        return ExitCode::from(plugin::EXIT_FAILURE);
    }
    let request: Request = match serde_json::from_slice(&input) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("burrow-engine-antigravity: failed to decode plugin request: {e}");
            return ExitCode::from(plugin::EXIT_FAILURE);
        }
    };
    let result = plugin::handle_request(&config, &request, &|| false);
    if let Some(diag) = &result.diagnostic
        && let Ok(line) = serde_json::to_string(diag)
    {
        eprintln!("{line}");
    }
    if let Some(response) = &result.response {
        let written = serde_json::to_vec(response)
            .map_err(|e| format!("failed to encode plugin response: {e}"))
            .and_then(|bytes| {
                let mut out = io::stdout().lock();
                out.write_all(&bytes)
                    .and_then(|()| out.write_all(b"\n"))
                    .and_then(|()| out.flush())
                    .map_err(|e| format!("failed to write plugin response: {e}"))
            });
        if let Err(why) = written {
            eprintln!("burrow-engine-antigravity: {why}");
            return ExitCode::from(plugin::EXIT_FAILURE);
        }
    }
    ExitCode::from(result.exit_code)
}
