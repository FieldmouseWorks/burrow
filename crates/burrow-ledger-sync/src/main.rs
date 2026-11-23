//! burrow-ledger-sync CONFIG [OWNER/REPO]: see the library's docs.

use std::path::PathBuf;
use std::process::ExitCode;

use burrow_ledger_sync::{Config, run};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let (config, only) = match &args[..] {
        [_, config] => (config, None),
        [_, config, repo] => (config, Some(repo.clone())),
        _ => {
            eprintln!(
                "burrow-ledger-sync CONFIG [OWNER/REPO]: bring every repo of CONFIG's owners,\n\
                 or the one given, up to date in the ledger, with one cn sync."
            );
            return ExitCode::from(2);
        }
    };
    let config: Config = match std::fs::read_to_string(config)
        .map_err(|e| e.to_string())
        .and_then(|text| serde_json::from_str(&text).map_err(|e| e.to_string()))
    {
        Ok(config) => config,
        Err(e) => {
            eprintln!("{config}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let Some(ledger) = std::env::var_os("CONARY_LEDGER").map(PathBuf::from) else {
        eprintln!("CONARY_LEDGER isn't set: where is the ledger?");
        return ExitCode::FAILURE;
    };
    if run(&config, &ledger, only) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
