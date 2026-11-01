//! `burrow-upkeep CONFIG settle|branches|watch ...`, run by a pass of the autoland
//! steward (modules/autoland/steward.py) for each repo, with the App's token
//! for the repo's owner in GH_TOKEN, under the pass's lock; `post`, which
//! posts a reviewer's answer on a repo's PR; `verdict`, which reads what
//! its reviewers said of one back; `queue`, which queues a
//! reviewer on one; `claims`, which sees to the steward's Claude jobs that
//! settle a PR, posting each one's answers once its push is the PR's head;
//! `fetch`, which fetches refs into the steward's mirror of a
//! repo, and `sweep`, which deletes what the reviewers leave of its closed
//! PRs and old heads; `token`, which mints the pass
//! that token; and `hooks`, which a full pass ends with, and which mints its
//! own. What it says of itself, on stderr, goes to the steward's journal.

use std::collections::{BTreeSet, HashMap};
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use burrow_github::Gh;
use burrow_upkeep::branches::{self, Git};
use burrow_upkeep::claims;
use burrow_upkeep::config::Config;
use burrow_upkeep::hook::{self, Hooks};
use burrow_upkeep::mirror;
use burrow_upkeep::pass::{Out, Pass, Proj};
use burrow_upkeep::post::{self, Look, Posted, Unposted};
use burrow_upkeep::queue::{self, Ask};
use burrow_upkeep::settle::Repo;
use burrow_upkeep::sweep;
use burrow_upkeep::token::{self, Installations, Token};
use burrow_upkeep::verdict::{self, Asked};
use gh_token::app::Minted;
use serde::de::DeserializeOwned;
use serde_json::json;

const USAGE: &str = "usage:
  burrow-upkeep CONFIG token OWNER
      a token of the App's installation on OWNER, minted with its key
      (systemd's credential app-key), and when it expires, as JSON on
      stdout; exit status 75 when GitHub can't be asked just now
  burrow-upkeep CONFIG settle OWNER/REPO --now SECONDS [--private] [--empty] [--dry]
      the repo's labels, settings and webhook, if they're due, as of the
      pass's time: what failed and a rate limit's reset, as JSON on stdout;
      --private, --empty: the repo is private, has no commit yet
  burrow-upkeep CONFIG hooks --now SECONDS
      a full pass's webhooks: the projects' repos', the ones off the repos
      no longer seen to, and the App's own, as the App's installation on
      each owner; a JSON object on stdin lists the repos the pass sees to
      (seen) and the projects' repos (projects): what failed and a rate
      limit's reset, as JSON on stdout
  burrow-upkeep CONFIG branches OWNER/REPO --now SECONDS --default BRANCH [--full] [--dry]
      the repo's branches nothing needs any more, deleted, leaving those its
      open PRs have (head and base), a JSON list on stdin: what's stuck and
      the pass to book, as JSON on stdout; --default, its default branch;
      --full, a full pass's look, which tries again what GitHub said no to
  burrow-upkeep CONFIG post OWNER/REPO --now SECONDS
      a reviewer's answer, posted as its review of the head it read; a JSON
      object on stdin gives the PR's number, the head, its base, the
      reviewer (and its name), its answer, the description it was shown and
      the PR's now: what came of it (clean, findings, notAReview, refused
      or unanswered), whether its review covers the PR's description, and a
      rate limit's reset, as JSON on stdout
  burrow-upkeep CONFIG verdict OWNER/REPO --now SECONDS [--dry]
      what each of a PR's reviewers last said of its head, read back from
      the reviews the steward posted; a JSON object on stdin gives the
      head, its base, the fingerprint of its description, the reviewers and
      the PR's reviews: each one's verdict, when, and its rounds with
      findings, or why it couldn't tell, as JSON on stdout
  burrow-upkeep CONFIG queue OWNER/REPO --now SECONDS
      a reviewer queued on a PR's head, with its snapshot and its brief; a
      JSON object on stdin gives the PR's number, the head, its base, its
      title, its description (quoted, and its fingerprint), when it was
      opened, the reviewer, the snapshot's name and the reasons' weighing:
      the queue entry's name or why it couldn't be queued, and a rate
      limit's reset, as JSON on stdout
  burrow-upkeep CONFIG claims OWNER/REPO --now SECONDS
      a PR's steward Claude jobs that settle it, seen to; a JSON object on
      stdin gives the PR's number, its head and branch, the login its jobs
      push as (or null), proj's jobs dir, its jobs as its state keeps them
      and who pushed a head as last found: each one that's over has its
      answers posted only while the PR's head is its push (or the head it
      was handed, when it pushed none), and each at work on a head someone
      else moved the PR past is named to stop; the jobs to keep, who pushed
      the head, the jobs to stop, what came of each one's answers (posted,
      wrong, unpushed, moved, refused or unanswered), when to look again and
      a rate limit's reset, as JSON on stdout
  burrow-upkeep CONFIG sweep OWNER/REPO --now SECONDS
      what the reviewers leave of the repo's PRs, deleted once nothing needs
      it, a JSON list on stdin giving the snapshots of its open PRs' heads:
      what it left (the looks under way, the sessions it couldn't mark gone)
      or why it couldn't sweep, as JSON on stdout
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
        Err(Stop::Later(why)) => {
            eprintln!("burrow-upkeep: {why}");
            // sysexits' EX_TEMPFAIL, as gh-token's: ask again later.
            ExitCode::from(75)
        }
    }
}

enum Stop {
    Usage(String),
    Failed(String),
    Later(String),
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
    if let [word, owner] = words
        && word == "token"
    {
        let cfg = Config::read(Path::new(config)).map_err(Stop::Failed)?;
        let m = app_token(&cfg, owner)?;
        println!("{}", json!({"token": m.token, "expires": m.expires}));
        return Ok(());
    }
    if let [word, rest @ ..] = words
        && word == "hooks"
    {
        return hooks(config, rest);
    }
    let (word, repo, rest) = match words {
        [word, repo, rest @ ..] => (word.as_str(), repo.as_str(), rest),
        _ => return Err(Stop::Usage("no word and repo".into())),
    };
    if !flaky::quarantine::repo_name(repo) {
        return Err(Stop::Usage(format!("{repo:?} isn't OWNER/REPO")));
    }
    if ![
        "settle", "branches", "watch", "post", "verdict", "queue", "claims", "fetch", "sweep",
    ]
    .contains(&word)
    {
        return Err(Stop::Usage(format!("no word {word:?}")));
    }
    let f = flags(rest)?;
    if let Some((flag, its)) = f.own.iter().find(|(_, its)| *its != word) {
        return Err(Stop::Usage(format!("{flag} is {its}'s")));
    }
    if f.dry && ["post", "claims"].contains(&word) {
        return Err(Stop::Usage(format!(
            "{word} has no status run: it posts what it's given"
        )));
    }
    if f.dry && word == "queue" {
        return Err(Stop::Usage(
            "queue has no status run: it writes the steward's mirror and the queue".into(),
        ));
    }
    if word == "fetch" {
        return fetched(config, repo, f.dry);
    }
    if word == "sweep" {
        return swept(config, repo, f.dry);
    }
    let look = match word {
        "post" => Some(stdin::<Look>(
            "the look to post, as a JSON object on stdin",
        )?),
        _ => None,
    };
    if let Some(l) = &look
        && !verdict::named(&l.reviewer)
    {
        return Err(Stop::Usage(format!(
            "{:?} isn't a reviewer's name",
            l.reviewer
        )));
    }
    let asked = match word {
        "verdict" => Some(stdin::<Asked>(
            "the PR and its reviews, as a JSON object on stdin",
        )?),
        _ => None,
    };
    if let Some(why) = asked.as_ref().and_then(Asked::wrong) {
        return Err(Stop::Usage(why));
    }
    let ask = match word {
        "queue" => Some(stdin::<Ask>(
            "the reviewer to queue, as a JSON object on stdin",
        )?),
        _ => None,
    };
    if let Some(why) = ask.as_ref().and_then(Ask::wrong) {
        return Err(Stop::Usage(why));
    }
    let claimed = match word {
        "claims" => Some(stdin::<claims::Look>(
            "the PR and its Claude jobs, as a JSON object on stdin",
        )?),
        _ => None,
    };
    let open = match word {
        "branches" => Some(stdin::<BTreeSet<String>>(
            "the open PRs' branches, as a JSON list on stdin",
        )?),
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
        api: &mut Gh::default(),
        jobs: &mut proj,
        out: Out::default(),
    };
    let out = match (word, open) {
        ("settle", _) => {
            let repo = Repo {
                private: f.private,
                empty: f.empty,
            };
            burrow_upkeep::settle(&mut p, &repo, || secret(&creds()?));
            json!({"failed": p.out.failed, "limited": p.out.limited})
        }
        ("post", _) => {
            let Some(look) = &look else {
                return Err(Stop::Usage("no look to post".into()));
            };
            let (outcome, why) = match post::post(&mut p, look) {
                Ok(Posted::Clean) => ("clean", None),
                Ok(Posted::Findings) => ("findings", None),
                Err(Unposted::NotAReview(why)) => ("notAReview", Some(why)),
                Err(Unposted::Refused(why)) => ("refused", Some(why)),
                Err(Unposted::Unanswered(why)) => ("unanswered", Some(why)),
            };
            let covers = verdict::covers(look.description.as_deref(), &look.now);
            json!({"outcome": outcome, "covers": covers, "why": why, "limited": p.out.limited})
        }
        ("verdict", _) => {
            let Some(asked) = &asked else {
                return Err(Stop::Usage("no PR to read".into()));
            };
            match verdict::said(&p.me(), asked) {
                Ok(said) => json!({"said": said, "why": null, "limited": null}),
                Err(why) => json!({"said": null, "why": why, "limited": null}),
            }
        }
        ("queue", _) => {
            let Some(ask) = &ask else {
                return Err(Stop::Usage("no reviewer to queue".into()));
            };
            let (queued, why) = match queue::queue(&mut p, ask) {
                Ok(entry) => (Some(entry), None),
                Err(why) => (None, Some(why)),
            };
            json!({"queued": queued, "why": why, "limited": p.out.limited})
        }
        ("claims", _) => {
            let Some(l) = &claimed else {
                return Err(Stop::Usage("no PR to see to".into()));
            };
            let seen = claims::claims(&mut p, l);
            let mut out = serde_json::to_value(&seen).map_err(|e| Stop::Failed(e.to_string()))?;
            out["wake"] = json!(p.out.wake);
            out["limited"] = json!(p.out.limited);
            out
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

/// What the steward hands the word on stdin, `what` it is: the open PRs'
/// branches, none of which is deleted, the look whose answer is posted, the
/// PR whose verdicts are read, the reviewer to queue, a PR's Claude jobs, the refs to fetch, the open PRs' snapshots, or the
/// repos a full pass's hooks are about. What isn't stops the word before it starts.
fn stdin<T: DeserializeOwned>(what: &str) -> Result<T, Stop> {
    let mut text = String::new();
    std::io::stdin()
        .read_to_string(&mut text)
        .map_err(|e| Stop::Failed(format!("stdin: {e}")))?;
    serde_json::from_str(&text).map_err(|e| Stop::Usage(format!("{what}: {e}")))
}

/// `fetch`, which a status run never asks for: the steward's mirror is
/// fetched into by the steward alone. It doesn't talk to GitHub's API, so
/// it never meets its rate limit.
fn fetched(config: &str, repo: &str, dry: bool) -> Result<(), Stop> {
    if dry {
        return Err(Stop::Usage(
            "fetch has no status run: it writes the steward's mirror".into(),
        ));
    }
    let refs: Vec<String> = stdin("the refspecs to fetch, as a JSON list on stdin")?;
    let cfg = Config::read(Path::new(config)).map_err(Stop::Failed)?;
    let out = match mirror::fetch(&cfg, &cfg.state_dir, repo, &refs) {
        Ok(m) => json!({"mirror": m, "why": null, "limited": null}),
        Err(why) => json!({"mirror": null, "why": why, "limited": null}),
    };
    println!("{out}");
    Ok(())
}

/// `sweep`, which a status run never asks for: it deletes the steward's
/// state. It asks GitHub nothing, so it's handed no token.
fn swept(config: &str, repo: &str, dry: bool) -> Result<(), Stop> {
    if dry {
        return Err(Stop::Usage(
            "sweep has no status run: it deletes what the reviewers leave".into(),
        ));
    }
    let live: BTreeSet<String> =
        stdin("the snapshots of the open PRs' heads, as a JSON list on stdin")?;
    let key = repo.replace('/', "_");
    if let Some(why) = sweep::wrong(&key, &live) {
        return Err(Stop::Usage(why));
    }
    let cfg = Config::read(Path::new(config)).map_err(Stop::Failed)?;
    let out = match sweep::sweep(&cfg.state_dir, &cfg.snapshot.dir, &key, &live) {
        Ok(left) => json!({"left": left, "why": null, "limited": null}),
        Err(why) => json!({"left": null, "why": why, "limited": null}),
    };
    println!("{out}");
    Ok(())
}

/// systemd's credentials for the pass: the App's key (app-key) and the
/// hooks' secret (webhook).
fn creds() -> Result<PathBuf, String> {
    std::env::var_os("CREDENTIALS_DIRECTORY")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| {
            "no CREDENTIALS_DIRECTORY: the App's key and the hooks' secret are credentials".into()
        })
}

/// The hooks' secret, burrow-events': GitHub signs each delivery with it.
fn secret(creds: &Path) -> Result<String, String> {
    let path = creds.join("webhook");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("the hooks' secret, {}: {e}", path.display()))?;
    Ok(text.trim().to_string())
}

/// Unix seconds, by the clock: what the App's JWTs and tokens are judged by.
fn clock() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// A full pass's hooks (hook.rs), as the App: on stdout, what failed and a
/// rate limit's reset. A status run has no secret, and no hooks to change.
fn hooks(config: &str, rest: &[String]) -> Result<(), Stop> {
    let f = flags(rest)?;
    if let Some((flag, its)) = f.own.first() {
        return Err(Stop::Usage(format!("{flag} is {its}'s")));
    }
    if f.dry {
        return Err(Stop::Usage(
            "hooks has no status run: it changes what it looks at".into(),
        ));
    }
    let repos: hook::Repos =
        stdin("the repos the pass sees to and the projects', as a JSON object on stdin")?;
    let cfg = Config::read(Path::new(config)).map_err(Stop::Failed)?;
    let creds = creds().map_err(Stop::Failed)?;
    let secret = secret(&creds).map_err(Stop::Failed)?;
    let app = token::app(&cfg, &creds, ureq::Proxy::try_from_env())
        .map_err(|e| Stop::Failed(e.to_string()))?;
    let now = clock();
    let mut owners = Installations {
        app: &app,
        now,
        gh: HashMap::new(),
    };
    let mut h = Hooks {
        cfg: &cfg,
        now: f.now,
        secret: &secret,
        owners: &mut owners,
        app: &app,
        clock: now,
        out: Out::default(),
        said: vec![],
    };
    h.run(&repos);
    println!(
        "{}",
        json!({"failed": h.out.failed, "limited": h.out.limited})
    );
    Ok(())
}

/// The App's token for `owner`, as of the clock now. An owner the App
/// isn't installed on has none: GitHub's answer, said as such.
fn app_token(cfg: &Config, owner: &str) -> Result<Minted, Stop> {
    let creds: OsString = creds().map_err(Stop::Failed)?.into();
    let proxy = ureq::Proxy::try_from_env();
    let app = token::app(cfg, Path::new(&creds), proxy)
        .map_err(|e| Stop::Failed(format!("no token for {owner}: {e}")))?;
    match token::token(&app, owner, clock()) {
        Ok(Token::Minted(m)) => Ok(m),
        Ok(Token::NotInstalled) => Err(Stop::Failed(format!(
            "the App {} isn't installed on {owner}",
            cfg.app.slug
        ))),
        Err(e) if e.unavailable => Err(Stop::Later(format!("no token for {owner}: {e}"))),
        Err(e) => Err(Stop::Failed(format!("no token for {owner}: {e}"))),
    }
}
