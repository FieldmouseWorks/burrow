//! burrow-keep-prune before|after, as flake-check.yml's keep steps run it on
//! a push to the default branch: `before` the leg's save says whether a newer
//! commit's entry is there already (`superseded=true` in GITHUB_OUTPUT, and
//! the save is skipped), `after` it deletes the entries it supersedes
//! (burrow_keep::prune says which). KEPT_KEY is the leg's key without the
//! commit, GITHUB_SHA the commit, DEFAULT_BRANCH the branch; gh asks GitHub
//! with GH_TOKEN, which `after` needs actions: write for.

use std::env;
use std::fs::OpenOptions;
use std::io::Write;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use burrow_github::Gh;
use burrow_keep::prune::Leg;

fn var(k: &str) -> Result<String, String> {
    env::var(k)
        .ok()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| format!("{k} is not set"))
}

fn run(mode: &str) -> Result<(), String> {
    let (repo, branch, prefix, sha) = (
        var("GITHUB_REPOSITORY")?,
        var("DEFAULT_BRANCH")?,
        var("KEPT_KEY")?,
        var("GITHUB_SHA")?,
    );
    let leg = Leg {
        repo: &repo,
        branch: &branch,
        prefix: &prefix,
        sha: &sha,
    };
    // Keeping them is for later runs' speed: no step of it holds the job long.
    let mut api = Gh {
        until: Some(Instant::now() + Duration::from_secs(600)),
        ..Gh::default()
    };
    match mode {
        "before" => match leg.superseded(&mut api)? {
            Some(newer) => {
                println!("{newer} is a newer commit's for this leg, so this push's isn't saved");
                let path = var("GITHUB_OUTPUT")?;
                let mut out = OpenOptions::new()
                    .append(true)
                    .create(true)
                    .open(&path)
                    .map_err(|e| format!("{path}: {e}"))?;
                writeln!(out, "superseded=true").map_err(|e| format!("{path}: {e}"))?;
            }
            None => println!("no newer commit's entry for this leg: saving {prefix}{sha}"),
        },
        "after" => {
            let pruned = leg.prune(&mut api)?;
            if pruned.deleted.is_empty() {
                println!("no entry of this leg's is superseded");
            }
            for key in &pruned.deleted {
                println!("deleted {key}");
            }
            if pruned.own {
                println!(
                    "a newer commit's entry for this leg turned up first, so this push's went too"
                );
            }
        }
        _ => return Err(format!("usage: burrow-keep-prune before|after, not {mode}")),
    }
    Ok(())
}

fn main() -> ExitCode {
    let mode = env::args().nth(1).unwrap_or_default();
    match run(&mode) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("burrow-keep-prune: {e}");
            ExitCode::FAILURE
        }
    }
}
