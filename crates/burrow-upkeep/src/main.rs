//! `burrow-upkeep CONFIG settle|branches|watch ...`, run by a pass of the autoland
//! steward (modules/autoland/steward.py) for each repo, with the App's token
//! for the repo's owner in GH_TOKEN, under the pass's lock. What it says of
//! itself, on stderr, goes to the steward's journal.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::Path;
use std::process::ExitCode;

use burrow_github::Gh;
use burrow_upkeep::branches::{self, Git};
use burrow_upkeep::config::Config;
use burrow_upkeep::pass::{Out, Pass, Proj};
use burrow_upkeep::settle::{self, Repo};
use serde_json::json;

const USAGE: &str = "usage:
  burrow-upkeep CONFIG settle OWNER/REPO --now SECONDS [--private] [--empty] [--dry]
      the repo's labels and settings, if they're due, as of the pass's time:
      whether they were, what failed and a rate limit's reset, as JSON on
      stdout; --private, --empty: the repo is private, has no commit yet
  burrow-upkeep CONFIG branches OWNER/REPO --now SECONDS --default BRANCH [--full] [--dry]
      the repo's branches nothing needs any more, deleted, leaving those its
      open PRs have (head and base), a JSON list on stdin: what's stuck and
      the pass to book, as JSON on stdout; --default, its default branch;
      --full, a full pass's look, which tries again what GitHub said no to
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

/// What follows the words: --now (required) and --dry, then each word's
/// own (`own`: the flag, and its word).
#[derive(Default)]
struct Flags {
    now: f64,
    dry: bool,
    searched: Option<usize>,
    private: bool,
    empty: bool,
    default: Option<String>,
    full: bool,
    own: Vec<(String, &'static str)>,
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
            "--default" => f.default = Some(value()?.clone()),
            "--dry" => f.dry = true,
            "--private" => f.private = true,
            "--empty" => f.empty = true,
            "--full" => f.full = true,
            _ => return Err(Stop::Usage(format!("unexpected {flag:?}"))),
        }
        let word = match flag.as_str() {
            "--searched" => "watch",
            "--private" | "--empty" => "settle",
            "--default" | "--full" => "branches",
            _ => continue,
        };
        f.own.push((flag.clone(), word));
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
    if !["settle", "branches", "watch"].contains(&word) {
        return Err(Stop::Usage(format!("no word {word:?}")));
    }
    let f = flags(rest)?;
    if let Some((flag, its)) = f.own.iter().find(|(_, its)| *its != word) {
        return Err(Stop::Usage(format!("{flag} is {its}'s")));
    }
    let open = match word {
        "branches" => Some(open()?),
        _ => None,
    };
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
    let out = match (word, open) {
        ("settle", _) => {
            let repo = Repo {
                private: f.private,
                empty: f.empty,
            };
            let due = settle::settle(&mut p, &cfg.settle, &repo);
            json!({"due": due, "failed": p.out.failed, "limited": p.out.limited})
        }
        ("branches", Some(open)) => {
            let default = f
                .default
                .ok_or_else(|| Stop::Usage("--default is needed".into()))?;
            if let Some(b) = &cfg.branches {
                let repo = branches::Repo {
                    default: &default,
                    open: &open,
                    full: f.full,
                };
                branches::watch(&mut p, b, &repo, &mut Git { bin: "git".into() });
            }
            let o = &p.out;
            json!({"stuck": o.stuck, "wake": o.wake, "failed": o.failed, "limited": o.limited})
        }
        _ => {
            burrow_upkeep::watch(&mut p);
            serde_json::to_value(&p.out).map_err(|e| Stop::Failed(e.to_string()))?
        }
    };
    println!("{out}");
    Ok(())
}

/// The open PRs' branches, as the steward gives them: none is deleted, so
/// what isn't a list of them stops the look before it starts.
fn open() -> Result<BTreeSet<String>, Stop> {
    let mut text = String::new();
    std::io::stdin()
        .read_to_string(&mut text)
        .map_err(|e| Stop::Failed(format!("stdin: {e}")))?;
    serde_json::from_str(&text).map_err(|e| {
        Stop::Usage(format!(
            "the open PRs' branches, as a JSON list on stdin: {e}"
        ))
    })
}
