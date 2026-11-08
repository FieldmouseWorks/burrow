use burrow_engines_door::{Config, pass};
use burrow_github::Gh;
use std::{fs, process::ExitCode};
fn main() -> ExitCode {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let [path] = args.as_slice() else {
        eprintln!("usage: burrow-engines-door CONFIG");
        return ExitCode::from(2);
    };
    let result = fs::read(path)
        .map_err(|e| e.to_string())
        .and_then(|s| serde_json::from_slice::<Config>(&s).map_err(|e| e.to_string()))
        .and_then(|cfg| pass(&mut Gh::default(), &cfg));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("burrow-engines-door: {e}");
            ExitCode::FAILURE
        }
    }
}
