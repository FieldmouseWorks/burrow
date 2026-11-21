//! burrow-review-ask, in four parts (lib.rs, broker.rs):
//!
//! - `door REPO PR HEAD`: ask Codex's connector to review the head as the
//!   owner, if every check holds (lib.rs). Its config is the file
//!   BURROW_REVIEW_ASK_CONFIG names. The broker runs it.
//! - `broker CONFIG`: the broker, which runs the door for the users it's
//!   given to, and for no one else, each door in a fence of its own.
//! - `enter FENCE -- DOOR ARGS...`: the door, as the broker starts it, once
//!   this process is inside its fence (burrow_process::fence).
//! - `ask SOCKET REPO PR HEAD`: ask through the broker, as the steward and
//!   Conary's flow do.
//!
//! The door and the client print what came of it as one JSON line (lib.rs
//! `Outcome`) and exit as `Outcome::code` says.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use burrow_github::Gh;
use burrow_review_ask::{Config, Outcome, ask, broker};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (code, said) = match args.split_first() {
        Some((what, rest)) if what == "door" => {
            let out = door(rest);
            (
                out.code(),
                serde_json::to_string(&out).expect("an outcome is JSON"),
            )
        }
        Some((what, rest)) if what == "enter" => {
            let out = match rest {
                [fence, dashes, program, args @ ..] if dashes == "--" => Outcome::Failed {
                    why: format!(
                        "the door in its fence {fence}: {}",
                        burrow_process::fence::enter(fence.as_ref(), program, args)
                    ),
                },
                _ => Outcome::Failed {
                    why: "usage: burrow-review-ask enter FENCE -- DOOR ARGS...".into(),
                },
            };
            (
                out.code(),
                serde_json::to_string(&out).expect("an outcome is JSON"),
            )
        }
        Some((what, rest)) if what == "ask" => {
            let reply = broker::client(rest);
            (reply.code, reply.said)
        }
        Some((what, rest)) if what == "broker" => match broker::main(rest) {
            Ok(()) => return,
            Err(why) => {
                eprintln!("burrow-review-ask broker: {why}");
                std::process::exit(1);
            }
        },
        _ => {
            let out = Outcome::Failed {
                why: "usage: burrow-review-ask door|ask|broker|enter ...".into(),
            };
            (
                out.code(),
                serde_json::to_string(&out).expect("an outcome is JSON"),
            )
        }
    };
    println!("{said}");
    std::process::exit(code);
}

fn door(args: &[String]) -> Outcome {
    let [repo, pr, head] = args else {
        return Outcome::Failed {
            why: "usage: burrow-review-ask door REPO PR HEAD".into(),
        };
    };
    let Ok(pr) = pr.parse() else {
        return Outcome::Failed {
            why: format!("{pr:?} isn't a PR number"),
        };
    };
    // It acts as the user's own gh login, as a person's job does: a token
    // or an App's socket in its environment would make it someone else,
    // whose ask the connector ignores.
    if ["GH_TOKEN", "GITHUB_TOKEN", "GH_TOKEN_SOCKET"]
        .iter()
        .any(|v| std::env::var_os(v).is_some_and(|v| !v.is_empty()))
    {
        return Outcome::Failed {
            why: "it runs as the user's own gh login, and was given a token or an App's socket"
                .into(),
        };
    }
    let cfg: Config = match std::env::var("BURROW_REVIEW_ASK_CONFIG")
        .map_err(|e| format!("BURROW_REVIEW_ASK_CONFIG: {e}"))
        .and_then(|p| std::fs::read_to_string(&p).map_err(|e| format!("{p}: {e}")))
        .and_then(|s| serde_json::from_str(&s).map_err(|e| format!("its config: {e}")))
    {
        Ok(c) => c,
        Err(why) => return Outcome::Failed { why },
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Bounded: the broker kills it past broker::DOOR.
    let mut gh = Gh {
        until: Some(Instant::now() + Duration::from_secs(120)),
        ..Gh::default()
    };
    ask(&mut gh, &cfg, now, repo, pr, head)
}
