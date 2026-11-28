//! burrow-keep-upload URL, as flake-check.yml's builds-write step runs it
//! once the gate ran: what this run built, to the build store at URL (its
//! substituter's URL; the door is URL/upload), with this job's OIDC token
//! (crates/burrow-keep/src/upload.rs). A job with no token (a PR from a
//! fork) can't write, and says so; a job the door refuses says why, and
//! neither fails. Anything else that stops it does, for the step to say
//! the store wasn't written (degraded/builds-write).

use std::env;
use std::io::Write;
use std::process::ExitCode;

use burrow_keep::nix;
use burrow_keep::upload::{Dump, Http, summary, upload};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let [url] = args.as_slice() else {
        eprintln!("usage: burrow-keep-upload URL");
        return ExitCode::FAILURE;
    };
    let var = |k: &str| env::var(k).unwrap_or_default();
    let (token_url, bearer) = (
        var("ACTIONS_ID_TOKEN_REQUEST_URL"),
        var("ACTIONS_ID_TOKEN_REQUEST_TOKEN"),
    );
    let line = if token_url.is_empty() || bearer.is_empty() {
        "build store: this job has no OIDC token, so it can't write the store".to_owned()
    } else {
        let run = || -> Result<String, String> {
            let door = Http::new(url, &token_url, &bearer)?;
            let store = nix::store()?;
            Ok(summary(&upload(&store, &Dump, &door)?))
        };
        match run() {
            Ok(line) => line,
            Err(e) => {
                eprintln!("burrow-keep-upload: {e}");
                return ExitCode::FAILURE;
            }
        }
    };
    println!("{line}");
    if let Some(path) = env::var_os("GITHUB_STEP_SUMMARY")
        && let Ok(mut f) = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(path)
    {
        let _ = writeln!(f, "{line}");
    }
    ExitCode::SUCCESS
}
