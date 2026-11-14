//! burrow-codeql, as the shared code scanning's `pick` job runs it: `pick`,
//! for the event its environment names, writing the languages it picked to
//! GITHUB_OUTPUT as `languages`, the analyze job's matrix.

use std::env;
use std::fs::OpenOptions;
use std::io::Write;
use std::process::ExitCode;

use burrow_codeql::{Event, languages, matrix, pick};
use burrow_github::Gh;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    if args != ["pick"] {
        eprintln!("usage: burrow-codeql pick");
        return ExitCode::FAILURE;
    }
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            println!("::error::{e}");
            ExitCode::FAILURE
        }
    }
}

fn var(name: &str) -> Result<String, String> {
    env::var(name).map_err(|e| format!("{name}: {e}"))
}

fn run() -> Result<(), String> {
    // The project's languages first: one this can't analyze fails every
    // run, a push's as a PR's.
    let project = languages(&var("LANGUAGES")?)?;
    let event = if var("EVENT")? == "pull_request" {
        let pr = var("PR")?;
        Event::PullRequest(
            pr.parse()
                .map_err(|_| format!("PR: {pr:?} isn't a number"))?,
        )
    } else {
        Event::Push
    };
    let picked = pick(
        &mut Gh::default(),
        &var("GITHUB_REPOSITORY")?,
        &event,
        &project,
        &mut std::io::stdout().lock(),
    )?;
    let output = var("GITHUB_OUTPUT")?;
    let mut file = OpenOptions::new()
        .append(true)
        .create(true)
        .open(&output)
        .map_err(|e| format!("{output}: {e}"))?;
    writeln!(file, "languages={}", matrix(&picked)).map_err(|e| format!("{output}: {e}"))
}
