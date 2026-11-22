//! The example plugin's executable: one request on stdin, its answer on
//! stdout (Conary's docs/plugins.md).

use std::io::{self, Write};
use std::process::ExitCode;

fn main() -> ExitCode {
    let answered = serde_json::from_reader(io::stdin().lock())
        .map_err(|e| format!("the request doesn't read: {e}"))
        .and_then(|request| burrow_example_plugin::answer(&request))
        .and_then(|response| serde_json::to_vec(&response).map_err(|e| e.to_string()))
        .and_then(|bytes| io::stdout().write_all(&bytes).map_err(|e| e.to_string()));
    match answered {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("example plugin: {why}");
            ExitCode::FAILURE
        }
    }
}
