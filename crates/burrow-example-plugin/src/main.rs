//! The example plugin's executable: one request on stdin, its answer on
//! stdout (Conary's docs/plugins.md). With `EXAMPLE_PLUGIN_AROUND` set, the
//! fixture around that plugin (src/around.rs).

use burrow_example_plugin::{AROUND, answer, around};
use conary_core::plugin::Request;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let around_plugin = std::env::var_os(AROUND).map(PathBuf::from);
    let mut input = Vec::new();
    let answered = io::stdin()
        .lock()
        .read_to_end(&mut input)
        .map_err(|e| format!("the request doesn't read: {e}"))
        .and_then(|_| {
            serde_json::from_slice::<Request>(&input)
                .map_err(|e| format!("the request doesn't read: {e}"))
        })
        .and_then(|request| match &around_plugin {
            Some(plugin) if !around::looks(&request.task) => {
                around::pass(plugin, &input).map(|()| None)
            }
            Some(plugin) => answer(&request)
                .and_then(|response| around::as_plugin(plugin, response))
                .map(Some),
            None => answer(&request).map(Some),
        })
        .and_then(|response| match response {
            None => Ok(()),
            Some(response) => serde_json::to_vec(&response)
                .map_err(|e| e.to_string())
                .and_then(|bytes| io::stdout().write_all(&bytes).map_err(|e| e.to_string())),
        });
    match answered {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("example plugin: {why}");
            ExitCode::FAILURE
        }
    }
}
