//! `burrow-upkeep CONFIG watch|started ...`, run by a pass of the autoland
//! steward (modules/autoland/steward.py) for each repo, with the App's token
//! for the repo's owner in GH_TOKEN, under the pass's lock.

use std::path::Path;
use std::process::ExitCode;

use burrow_github::Gh;
use burrow_upkeep::config::Config;
use burrow_upkeep::pass::{Out, Pass, Proj};

const USAGE: &str = "usage:
  burrow-upkeep CONFIG watch OWNER/REPO --now SECONDS [--searched N] [--dry]
      the repo's dashboard and quarantine, as of the pass's time: what's
      stuck, the pass to book and the jobs to start, as JSON on stdout;
      --searched, how many repos the pass has searched for a dashboard
  burrow-upkeep CONFIG started OWNER/REPO dashboard|flaky JOB --now SECONDS [--dry]
      the job the last watch asked for has started
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

/// What follows the words: --now (required), --searched, --dry.
struct Flags {
    now: f64,
    searched: usize,
    dry: bool,
}

fn flags(rest: &[String]) -> Result<Flags, Stop> {
    let (mut now, mut searched, mut dry) = (None, 0, false);
    let mut it = rest.iter();
    while let Some(f) = it.next() {
        let mut value = || {
            it.next()
                .ok_or_else(|| Stop::Usage(format!("{f} needs a value")))
        };
        match f.as_str() {
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
                searched = v
                    .parse()
                    .map_err(|_| Stop::Usage(format!("--searched {v:?} isn't a count")))?;
            }
            "--dry" => dry = true,
            _ => return Err(Stop::Usage(format!("unexpected {f:?}"))),
        }
    }
    let now = now.ok_or_else(|| Stop::Usage("--now is needed".into()))?;
    Ok(Flags { now, searched, dry })
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
    let (started, rest) = match (word, rest) {
        ("watch", rest) => (None, rest),
        ("started", [part, jid, rest @ ..]) => (Some((part.as_str(), jid.as_str())), rest),
        ("started", _) => return Err(Stop::Usage("started needs a part and a job".into())),
        _ => return Err(Stop::Usage(format!("no word {word:?}"))),
    };
    let f = flags(rest)?;
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
        searched: f.searched,
        api: &mut Gh,
        jobs: &mut proj,
        out: Out::default(),
    };
    match started {
        None => {
            burrow_upkeep::watch(&mut p);
            let out = serde_json::to_string(&p.out).map_err(|e| Stop::Failed(e.to_string()))?;
            println!("{out}");
            Ok(())
        }
        Some((part, jid)) => burrow_upkeep::started(&mut p, part, jid).map_err(Stop::Failed),
    }
}
