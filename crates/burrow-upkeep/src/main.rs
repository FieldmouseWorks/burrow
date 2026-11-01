//! `burrow-upkeep CONFIG settle|watch ...`, run by a pass of the autoland
//! steward (modules/autoland/steward.py) for each repo, with the App's token
//! for the repo's owner in GH_TOKEN, under the pass's lock. What it says of
//! itself, on stderr, goes to the steward's journal.

use std::path::Path;
use std::process::ExitCode;

use burrow_github::Gh;
use burrow_upkeep::config::Config;
use burrow_upkeep::pass::{Out, Pass, Proj};
use burrow_upkeep::settle::{self, Repo};
use serde_json::json;

const USAGE: &str = "usage:
  burrow-upkeep CONFIG settle OWNER/REPO --now SECONDS [--private] [--empty] [--dry]
      the repo's labels and settings, if they're due, as of the pass's time:
      whether they were, what failed and a rate limit's reset, as JSON on
      stdout; --private, --empty: the repo is private, has no commit yet
  burrow-upkeep CONFIG watch OWNER/REPO --now SECONDS [--searched N] [--dry]
      the repo's dashboard and quarantine, as of the pass's time: what's
      stuck, the pass to book and the jobs to start, as JSON on stdout;
      --searched, how many repos the pass has searched for a dashboard
  --dry: a status run's, with state of its own";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(Stop::Usage(why)) => {
            eprintln!("burrow-upkeep: {why}\n{USAGE}");
            ExitCode::from(2)
        }
        Err(Stop::Failed(why)) => {
            eprintln!("burrow-upkeep: {why}");
            ExitCode::FAILURE
        }
    }
}

enum Stop {
    Usage(String),
    Failed(String),
}

/// What follows the words: --now (required) and --dry, then watch's
/// --searched and settle's --private and --empty.
#[derive(Default)]
struct Flags {
    now: f64,
    searched: Option<usize>,
    dry: bool,
    private: bool,
    empty: bool,
}

fn flags(rest: &[String]) -> Result<Flags, Stop> {
    let mut f = Flags::default();
    let mut now = None;
    let mut it = rest.iter();
    while let Some(flag) = it.next() {
        let mut value = || {
            it.next()
                .ok_or_else(|| Stop::Usage(format!("{flag} needs a value")))
        };
        match flag.as_str() {
            "--now" => {
                let v = value()?;
                now = Some(
                    v.parse::<f64>()
                        .ok()
                        .filter(|t| t.is_finite() && *t > 0.0)
                        .ok_or_else(|| Stop::Usage(format!("--now {v:?} isn't a time")))?,
                );
            }
            "--searched" => {
                let v = value()?;
                f.searched = Some(
                    v.parse()
                        .map_err(|_| Stop::Usage(format!("--searched {v:?} isn't a count")))?,
                );
            }
            "--dry" => f.dry = true,
            "--private" => f.private = true,
            "--empty" => f.empty = true,
            _ => return Err(Stop::Usage(format!("unexpected {flag:?}"))),
        }
    }
    f.now = now.ok_or_else(|| Stop::Usage("--now is needed".into()))?;
    Ok(f)
}

fn run(args: &[String]) -> Result<(), Stop> {
    let (config, words) = args
        .split_first()
        .ok_or_else(|| Stop::Usage("no config".into()))?;
    let (word, repo, rest) = match words {
        [word, repo, rest @ ..] => (word.as_str(), repo.as_str(), rest),
        _ => return Err(Stop::Usage("no word and repo".into())),
    };
    if !flaky::quarantine::repo_name(repo) {
        return Err(Stop::Usage(format!("{repo:?} isn't OWNER/REPO")));
    }
    let f = flags(rest)?;
    match word {
        "watch" if f.private || f.empty => {
            return Err(Stop::Usage("--private and --empty are settle's".into()));
        }
        "settle" if f.searched.is_some() => {
            return Err(Stop::Usage("--searched is watch's".into()));
        }
        "watch" | "settle" => {}
        _ => return Err(Stop::Usage(format!("no word {word:?}"))),
    }
    let cfg = Config::read(Path::new(config)).map_err(Stop::Failed)?;
    let mut proj = Proj {
        bin: cfg.proj.clone(),
        dry: f.dry,
    };
    let mut p = Pass {
        cfg: &cfg,
        repo,
        now: f.now,
        dry: f.dry,
        searched: f.searched.unwrap_or(0),
        api: &mut Gh,
        jobs: &mut proj,
        out: Out::default(),
    };
    let out = if word == "settle" {
        let repo = Repo {
            private: f.private,
            empty: f.empty,
        };
        let due = settle::settle(&mut p, &cfg.settle, &repo);
        json!({"due": due, "failed": p.out.failed, "limited": p.out.limited}).to_string()
    } else {
        burrow_upkeep::watch(&mut p);
        serde_json::to_string(&p.out).map_err(|e| Stop::Failed(e.to_string()))?
    };
    println!("{out}");
    Ok(())
}
