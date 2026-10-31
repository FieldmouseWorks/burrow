//! proj-inbox, as modules/proj.nix runs it: PROJ_CONFIG names proj's config,
//! whose inbox part is its own, and the inbox module's docs say what it does.

use std::process::ExitCode;

use burrow_proj::config::Config;
use burrow_proj::inbox;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let cfg = match Config::load() {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("proj-inbox: {e}");
            return ExitCode::FAILURE;
        }
    };
    match inbox::main(&cfg, &args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(fail) => {
            if let Some(say) = fail.say {
                eprintln!("{say}");
            }
            ExitCode::from(fail.code)
        }
    }
}
