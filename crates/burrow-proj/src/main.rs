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
    // Inside a job, the host's jobs are the host's to say, whatever the
    // job's own view of them (a sandbox's has none).
    if burrow_proj::host_jobs::about_jobs(&args)
        && std::env::var_os("PROJ_JOB").is_some_and(|j| !j.is_empty())
    {
        let socket = std::env::var_os("PROJ_JOBS_SOCKET").filter(|s| !s.is_empty());
        return burrow_proj::host_jobs::client(socket.as_deref().map(std::path::Path::new), &args);
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
