//! proj, as modules/proj.nix runs it: PROJ_CONFIG names its config, and the
//! crate's docs have its commands.

use std::process::ExitCode;

use burrow_proj::cmd::dispatch;
use burrow_proj::config::Config;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let cfg = match Config::load() {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("proj: {e}");
            return ExitCode::FAILURE;
        }
    };
    match dispatch(&cfg, &args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(fail) => {
            if let Some(say) = fail.say {
                eprintln!("{say}");
            }
            ExitCode::from(fail.code)
        }
    }
}
