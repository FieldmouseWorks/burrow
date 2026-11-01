//! burrow-update CONFIG WORD ...: HOST-update's steps in Rust, for update.sh
//! to run as root (lib.rs). modules/updates.nix builds it in with its config
//! as HOST-update-steps.
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
//! A step that won't go says why and exits 1: on stdout, where update.sh
//! puts that in what it says (red, ride, replaces, replace), else on
//! stderr, for the journal (pr, propose). 2 is a usage or a config it can't
//! take, which update.sh never gives: it never reads as a step going.

use std::ffi::OsString;
use std::path::Path;
use std::process::ExitCode;

use burrow_update::Config;
use burrow_update::train::{self, Pr};

const USAGE: &str = "pr | red PR | ride PR | replaces PR MAIN [INPUT...] | replace PR | propose";

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
    let usage = || {
        eprintln!("usage: {}-steps {USAGE}", cfg.update);
        ExitCode::from(2)
    };
    let Some(words) = words else { return usage() };
    let Some(t) = &cfg.train else {
        eprintln!(
            "{}: its PR is for a repo whose PRs land through its merge train, and this one's don't",
            cfg.update
        );
        return ExitCode::from(2);
    };
    let pr = |w: &str| Pr::parse(w);
    let mut gh = cfg.gh();
    match words[..] {
        ["pr"] => done(
            train::open(t, &mut gh).map(|p| p.map(|p| p.to_string())),
            Why::Logged,
        ),
        ["red", p] => {
            let Some(p) = pr(p) else { return usage() };
            done(
                train::red(&cfg, t, &mut gh, &p).map(|red| red.then(|| "red".into())),
                Why::Told,
            )
        }
        ["ride", p] => {
            let Some(p) = pr(p) else { return usage() };
            done(train::ride(&cfg, t, &p).map(|()| None), Why::Told)
        }
        ["replaces", p, main, ref short @ ..] => {
            let Some(p) = pr(p) else { return usage() };
            let short: Vec<String> = short.iter().map(|s| s.to_string()).collect();
            done(
                train::replaces(&cfg, &p, main, &short).map(|()| None),
                Why::Told,
            )
        }
        ["replace", p] => {
            let Some(p) = pr(p) else { return usage() };
            done(train::replace(&cfg, t, &mut gh, &p).map(Some), Why::Told)
        }
        ["propose"] => done(train::propose(&cfg, t, &mut gh).map(Some), Why::Logged),
        _ => usage(),
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
