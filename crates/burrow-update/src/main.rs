//! burrow-update CONFIG WORD ...: HOST-update's steps in Rust, for update.sh
//! to run as root (lib.rs). modules/updates.nix builds it in with its config
//! as HOST-update-steps.
//!
//! Its run: what runs and what's live (running.rs), the push (push.rs), the
//! VM tests (check.rs) and the fix jobs (dispatch.rs). What runs is what
//! update.sh found when its run began: CURRENT, the system, and REV, the
//! commit it's stamped with, or "" when that isn't one.
//!
//!   build                      HEAD's system, built: its store path; exit
//!                              1 when it doesn't build
//!   same CURRENT REV           exit 0 when HEAD's system is the one
//!                              running, apart from its stamp
//!   main-runs MAIN CURRENT REV exit 0 when main, MAIN and checked out, runs
//!   runs-already NEW CURRENT REV MADE RIDING
//!                              "up", "hold" or "current" when NEW, HEAD's
//!                              system, runs already; nothing when it
//!                              doesn't. MADE is "1" when HEAD is a commit
//!                              of this run's, RIDING its PR's number while
//!                              it rides it; each is "" otherwise
//!   live-through MAIN          how far main is live, once HEAD is
//!   live-at MAIN RUNNING       that main is live up to MAIN in RUNNING
//!   push-main                  HEAD, what went live, onto main
//!   check NAME COMMIT LOG      exit 0 when the VM test NAME passes on
//!                              COMMIT, with what it says in LOG
//!   dispatch KEY PROMPT        KEY's problem to a Claude job, unless a
//!                              recent one has it; exit 0 either way, as
//!                              update.sh goes on either way
//!
//! Its PR, in a train repo (train.rs):
//!
//!   pr                         its open PR, as "NUMBER HEAD", or nothing
//!                              when there's none; exit 1 when GitHub
//!                              doesn't say
//!   red PR                     "red" when PR ("NUMBER HEAD") is red on a
//!                              head it pushed, then fetched; nothing when
//!                              it isn't
//!   ride PR                    the clone, at main, with PR on top
//!   replaces PR MAIN [INPUT...]  exit 0 when HEAD, a fresh bump on MAIN
//!                              short of the INPUTs, replaces PR's red head
//!   replace PR                 HEAD as PR's new head, and PR's URL
//!   propose                    HEAD as its new PR, and that PR's URL
//!
//! A step that won't go exits 1, and says why: on stdout, where update.sh
//! puts that in what it says (red, ride, replaces, replace) or leaves it
//! for the journal (push-main), else on stderr, for the journal (pr,
//! propose, live-at); build leaves it to nix, in its log. 2 is a usage or a
//! config it can't take, which update.sh never gives: it never reads as a
//! step going.

use std::ffi::OsString;
use std::path::Path;
use std::process::ExitCode;

use burrow_update::running::{self, Running};
use burrow_update::train::{self, Pr};
use burrow_update::{Config, check, dispatch, is_commit, is_name, push};

const USAGE: &str = "build | same CURRENT REV | main-runs MAIN CURRENT REV \
| runs-already NEW CURRENT REV MADE RIDING | live-through MAIN | live-at MAIN RUNNING \
| push-main | check NAME COMMIT LOG | dispatch KEY PROMPT \
| pr | red PR | ride PR | replaces PR MAIN [INPUT...] | replace PR | propose";

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let Some((config, words)) = args.split_first() else {
        eprintln!("usage: burrow-update CONFIG {USAGE}");
        return ExitCode::from(2);
    };
    let cfg = match Config::read(Path::new(config)) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("burrow-update: {e}");
            return ExitCode::from(2);
        }
    };
    let words: Option<Vec<&str>> = words.iter().map(|w| w.to_str()).collect();
    words
        .and_then(|w| run(&cfg, &w).or_else(|| its_pr(&cfg, &w)))
        .unwrap_or_else(|| {
            eprintln!("usage: {}-steps {USAGE}", cfg.update);
            ExitCode::from(2)
        })
}

/// A step of its run, or None for words that aren't one.
fn run(cfg: &Config, words: &[&str]) -> Option<ExitCode> {
    let running = |current: &str, rev: &str| Running::parse(current, rev);
    fn commit(c: &str) -> Option<&str> {
        is_commit(c).then_some(c)
    }
    Some(match *words {
        ["build"] => match running::build(cfg) {
            Some(new) => {
                println!("{new}");
                ExitCode::SUCCESS
            }
            None => ExitCode::FAILURE,
        },
        ["same", current, rev] => yes(running::same_as_running(cfg, &running(current, rev)?)),
        ["main-runs", main, current, rev] => yes(running::main_runs(
            cfg,
            &running(current, rev)?,
            commit(main)?,
        )),
        ["runs-already", new, current, rev, made, riding] => {
            let made = match made {
                "" => false,
                "1" => true,
                _ => return None,
            };
            let riding = match riding {
                "" => false,
                n if n.bytes().all(|b| b.is_ascii_digit()) => true,
                _ => return None,
            };
            let run = running(current, rev)?;
            if let Some(a) = running::runs_already(cfg, &run, new, made, riding) {
                println!("{}", a.word());
            }
            ExitCode::SUCCESS
        }
        ["live-through", main] => {
            println!("{}", running::live_through(cfg, commit(main)?));
            ExitCode::SUCCESS
        }
        ["live-at", main, live] => done(
            running::live_at(cfg, commit(main)?, live).map(|()| None),
            Why::Logged,
        ),
        ["push-main"] => done(push::push_main(cfg).map(|()| None), Why::Told),
        ["check", name, c, log] if is_name(name) => {
            yes(check::run_check(cfg, name, commit(c)?, Path::new(log)))
        }
        ["dispatch", key, prompt] if is_name(key) => {
            if let Some(id) = dispatch::dispatch(cfg, key, prompt) {
                println!("handed to Claude: proj log {id}");
            }
            ExitCode::SUCCESS
        }
        _ => return None,
    })
}

/// A step of its PR, read from the words before anything else.
enum Step<'a> {
    Open,
    Red(Pr),
    Ride(Pr),
    Replaces(Pr, &'a str, Vec<String>),
    Replace(Pr),
    Propose,
}

/// A step of its PR, in a train repo, or None for words that aren't one.
fn its_pr(cfg: &Config, words: &[&str]) -> Option<ExitCode> {
    let pr = |w: &str| Pr::parse(w);
    let step = match *words {
        ["pr"] => Step::Open,
        ["red", p] => Step::Red(pr(p)?),
        ["ride", p] => Step::Ride(pr(p)?),
        ["replaces", p, main, ref short @ ..] => {
            Step::Replaces(pr(p)?, main, short.iter().map(|s| s.to_string()).collect())
        }
        ["replace", p] => Step::Replace(pr(p)?),
        ["propose"] => Step::Propose,
        _ => return None,
    };
    let Some(t) = &cfg.train else {
        eprintln!(
            "{}: its PR is for a repo whose PRs land through its merge train, and this one's don't",
            cfg.update
        );
        return Some(ExitCode::from(2));
    };
    let mut gh = cfg.gh();
    Some(match step {
        Step::Open => done(
            train::open(t, &mut gh).map(|p| p.map(|p| p.to_string())),
            Why::Logged,
        ),
        Step::Red(p) => done(
            train::red(cfg, t, &mut gh, &p).map(|red| red.then(|| "red".into())),
            Why::Told,
        ),
        Step::Ride(p) => done(train::ride(cfg, t, &p).map(|()| None), Why::Told),
        Step::Replaces(p, main, short) => done(
            train::replaces(cfg, &p, main, &short).map(|()| None),
            Why::Told,
        ),
        Step::Replace(p) => done(train::replace(cfg, t, &mut gh, &p).map(Some), Why::Told),
        Step::Propose => done(train::propose(cfg, t, &mut gh).map(Some), Why::Logged),
    })
}

/// A yes-or-no step's exit.
fn yes(yes: bool) -> ExitCode {
    if yes {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Where a step says why it won't go: stdout for update.sh to say it,
/// stderr for the journal.
#[derive(Clone, Copy)]
enum Why {
    Told,
    Logged,
}

/// A step's exit: what it gives on stdout, or why it won't go.
fn done(r: Result<Option<String>, String>, why: Why) -> ExitCode {
    match (r, why) {
        (Ok(out), _) => {
            out.into_iter().for_each(|o| println!("{o}"));
            ExitCode::SUCCESS
        }
        (Err(e), Why::Told) => {
            println!("{e}");
            ExitCode::FAILURE
        }
        (Err(e), Why::Logged) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
