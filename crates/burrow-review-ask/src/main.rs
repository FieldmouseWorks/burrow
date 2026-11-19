//! `burrow-review-ask REPO PR HEAD`: ask Codex's connector to review the
//! head as the owner, if every check holds (lib.rs). Its config is the file
//! BURROW_REVIEW_ASK_CONFIG names. Prints what came of it as one JSON line
//! (lib.rs `Outcome`) and exits 0, or 1 when it failed.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use burrow_github::Gh;
use burrow_review_ask::{Config, Outcome, ask};

fn main() {
    let out = run();
    println!(
        "{}",
        serde_json::to_string(&out).expect("an outcome is JSON")
    );
    std::process::exit(i32::from(matches!(out, Outcome::Failed { .. })));
}

fn run() -> Outcome {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [repo, pr, head] = args.as_slice() else {
        return Outcome::Failed {
            why: "usage: burrow-review-ask REPO PR HEAD".into(),
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
    // Bounded: the steward waits on it, in its pass.
    let mut gh = Gh {
        until: Some(Instant::now() + Duration::from_secs(120)),
        ..Gh::default()
    };
    ask(&mut gh, &cfg, now, repo, pr, head)
}
