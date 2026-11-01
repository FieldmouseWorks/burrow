//! What just went live, onto main. Main moves while an update runs (its VM
//! tests can outlast the gap between merges), so a push that gives up on
//! the first refusal can lose every bump of a busy night while each one
//! goes live. It goes on top of what landed instead, once that builds: a
//! clean rebase can still not. What went live is the only commit that marks
//! main as running it: one rebased here also carries what landed meanwhile,
//! which the push's own run puts through the VM tests and the health check
//! before anything goes live.

use std::fs;

use crate::Config;
use crate::running::build;

/// How many times main may move under the push before it gives up.
const TRIES: u32 = 3;

/// HEAD, what went live, pushed to main: `deployed-main` says main runs it
/// when it went as it was. Otherwise why it didn't go: the next run bumps
/// it afresh.
pub fn push_main(cfg: &Config) -> Result<(), String> {
    let live = cfg.git(&["rev-parse", "HEAD"])?;
    let mut tries = 0;
    for t in 1..=TRIES {
        tries = t;
        if cfg.git(&["push", "-q", "origin", "HEAD:main"]).is_ok() {
            if cfg.git(&["rev-parse", "HEAD"]).as_ref() == Ok(&live) {
                let _ = fs::write(cfg.state.join("deployed-main"), format!("{live}\n"))
                    .map_err(|e| eprintln!("{}: deployed-main: {e}", cfg.update));
            }
            return Ok(());
        }
        if cfg.git(&["fetch", "-q", "origin", "main"]).is_err() {
            break;
        }
        if cfg.git(&["rebase", "-q", "origin/main"]).is_err() {
            let _ = cfg.git(&["rebase", "--abort"]);
            return Err(format!(
                "main moved and this update doesn't go on top of it (try {t}); the next run bumps it afresh"
            ));
        }
        // Deploying main itself: nothing of its own to put on top.
        let head = cfg.git(&["rev-parse", "HEAD"]);
        if head.is_ok() && head == cfg.git(&["rev-parse", "origin/main"]) {
            return Ok(());
        }
        if build(cfg).is_none() {
            return Err(
                "main moved and this update doesn't build on top of it; the next run bumps it afresh"
                    .into(),
            );
        }
    }
    Err(format!(
        "push to main was refused {tries} times; the next run bumps it afresh"
    ))
}

#[cfg(test)]
#[path = "push_tests.rs"]
mod tests;
