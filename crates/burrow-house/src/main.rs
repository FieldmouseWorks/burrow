//! burrow-house, as burrow's house action runs it: `check`, for the PR its
//! event names, from the environment the action gives it.

use std::env;
use std::process::ExitCode;

use burrow_house::check::{self, Event};
use burrow_house::github::Real;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    if args != ["check"] {
        eprintln!("usage: burrow-house check");
        return ExitCode::FAILURE;
    }
    let ran = event().and_then(|event| {
        let gh = Real {
            repo: event.repo.clone(),
        };
        check::run(&event, &gh, &mut std::io::stdout().lock())
    });
    match ran {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn var(name: &str) -> Result<String, String> {
    env::var(name).map_err(|e| format!("{name}: {e}"))
}

fn number<T: std::str::FromStr>(name: &str) -> Result<T, String> {
    let value = var(name)?;
    value
        .parse()
        .map_err(|_| format!("{name}: {value:?} isn't a number"))
}

/// The event, from the action's environment and the runner's.
fn event() -> Result<Event, String> {
    let repo = var("GITHUB_REPOSITORY")?;
    let run_url = format!(
        "{}/{repo}/actions/runs/{}",
        var("GITHUB_SERVER_URL")?,
        var("GITHUB_RUN_ID")?
    );
    Ok(Event {
        number: number("NUMBER")?,
        head: var("HEAD_SHA")?,
        growth: number("GROWTH")?,
        lines: number("LINES")?,
        model_names: match var("MODEL_NAMES")?.as_str() {
            "true" => true,
            "false" => false,
            other => return Err(format!("MODEL_NAMES: {other:?} isn't true or false")),
        },
        repo,
        run_url,
    })
}
