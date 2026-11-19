//! `burrow-builds init CONFIG`: the store's directories, its key on first
//! start and its public half. `burrow-builds door CONFIG`: one upload, from
//! nginx, on stdin and stdout.

use std::io::{BufReader, BufWriter};
use std::process::ExitCode;

use burrow_builds::door::{self, Config};
use burrow_builds::{Key, Store};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [verb, path] = args.as_slice() else {
        eprintln!("usage: burrow-builds init|door CONFIG");
        return ExitCode::from(2);
    };
    let run = || -> Result<(), String> {
        let text = std::fs::read(path).map_err(|e| format!("{path} doesn't read: {e}"))?;
        let cfg: Config =
            serde_json::from_slice(&text).map_err(|e| format!("{path} doesn't read: {e}"))?;
        match verb.as_str() {
            "init" => {
                let key = Key::open_or_make(&cfg.key, &cfg.key_name)?;
                let store = Store {
                    dir: cfg.dir,
                    max_size: cfg.max_size,
                };
                store.init(&key).map_err(|e| e.to_string())
            }
            "door" => door::serve(
                &cfg,
                &mut BufReader::new(std::io::stdin().lock()),
                &mut BufWriter::new(std::io::stdout().lock()),
            )
            .map_err(|e| e.to_string()),
            other => Err(format!("{other} isn't init or door")),
        }
    };
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("burrow-builds: {e}");
            ExitCode::FAILURE
        }
    }
}
