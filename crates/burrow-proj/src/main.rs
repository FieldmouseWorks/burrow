//! proj, as modules/proj.nix runs it: PROJ_CONFIG names its config, and the
//! crate's docs have its commands.

use std::process::ExitCode;

use burrow_proj::cmd::dispatch;
use burrow_proj::config::Config;

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    if args.is_empty() {
        args.push("status".into());
    }
    if args.first().map(String::as_str) == Some("probe")
        && let Some(path) = std::env::var_os("PROJ_PROBE_SOCKET")
    {
        return match burrow_proj::probe::client(std::path::Path::new(&path), &args[1..]) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("proj probe: {e}");
                ExitCode::FAILURE
            }
        };
    }
    if burrow_proj::host_jobs::remote(&args)
        && std::env::var_os("PROJ_JOB").is_some_and(|id| !id.is_empty())
    {
        let result = match std::env::var_os("PROJ_HOST_JOBS_SOCKET") {
            Some(path) => burrow_proj::host_jobs::client(std::path::Path::new(&path), &args),
            None => Err(burrow_proj::Fail::new(
                1,
                "can't reach host jobs: this turn has no host job bridge; restart it on a deployment with host job access",
            )),
        };
        return match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(fail) => {
                if let Some(say) = fail.say {
                    eprintln!("proj: {say}");
                }
                ExitCode::from(fail.code)
            }
        };
    }
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
