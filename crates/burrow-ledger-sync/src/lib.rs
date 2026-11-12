//! burrow-ledger-sync: the ledger's pass (modules/ledger.nix). It sets the
//! ledger up until set-up has finished, then runs `cn sync REPO` for the one
//! repo it is given, or for every non-archived source repo of each owner.
//!
//! `cn` is the ledger's writer, which holds the token setup. When GitHub
//! couldn't be asked (its rate limit, its errors, the network: cn exits 75)
//! the repo's pass is booked five minutes on in burrow-events' wake file,
//! through burrow-github, and this pass has not failed: a failed unit is for
//! what retrying won't fix, which is what fails a switch that runs meanwhile.
//! Only a booking written makes it no failure.

use std::path::PathBuf;

use burrow_github::{Api, deferred, query};
use serde::Deserialize;
use serde_json::{Value, json};

/// What the pass is configured with, as modules/ledger.nix writes it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// `cn`, on the ledger, as its writers run it.
    pub cn: PathBuf,
    /// The ledger's directory: set up once `anchor` is in it.
    pub ledger: PathBuf,
    /// burrow-events' wake file for this handler.
    pub wake: PathBuf,
    /// Whose repos the full pass syncs.
    pub owners: Vec<String>,
}

/// What `cn` exits with when GitHub couldn't be asked.
pub const UNAVAILABLE: i32 = 75;

/// How long a repo waits when GitHub was unavailable.
pub const AGAIN: u64 = 300;

/// Runs `cn` with arguments, on the ledger: its exit code, None when a
/// signal ended it. Err when it couldn't be run at all.
pub trait Cn {
    fn run(&self, args: &[&str]) -> Result<Option<i32>, String>;
}

/// Sets the ledger up, until set-up has finished: its last step keeps the
/// anchor's copy beside the log. Run on a ledger that's set up, `cn init`
/// checks the anchor's whole history on GitHub, a call per commit on it, and
/// every sync adds one. A ledger whose key was lost fails here until the key
/// is back, and sync refuses one whose anchor moved.
pub fn set_up(cfg: &Config, cn: &dyn Cn) -> Result<(), String> {
    if cfg.ledger.join("anchor").exists() {
        return Ok(());
    }
    match cn.run(&["init"])? {
        Some(0) => Ok(()),
        other => Err(format!("cn init {}", how(other))),
    }
}

/// Syncs one repo; `now` is read when the booking is made, so the retry is
/// five minutes after the failure, however long the pass has run. GitHub being unavailable books its next pass and is no
/// failure; any other failure is one.
pub fn sync_repo(
    cfg: &Config,
    cn: &dyn Cn,
    repo: &str,
    now: &dyn Fn() -> u64,
) -> Result<(), String> {
    match cn.run(&["sync", repo])? {
        Some(0) => Ok(()),
        Some(UNAVAILABLE) => {
            eprintln!("{repo}: GitHub is unavailable; again in five minutes");
            deferred::book(&cfg.wake, repo, now() + AGAIN)
        }
        other => Err(format!("cn sync {repo} {}", how(other))),
    }
}

fn how(code: Option<i32>) -> String {
    code.map_or_else(
        || "was ended by a signal".to_owned(),
        |c| format!("exited {c}"),
    )
}

const REPOS: &str = "query($owner: String!, $after: String) { repositoryOwner(login: $owner) { \
    repositories(first: 100, after: $after, isArchived: false, isFork: false, \
    ownerAffiliations: OWNER) { nodes { nameWithOwner } \
    pageInfo { hasNextPage endCursor } } } }";

/// The non-archived source repos `owner` has (OWNER/NAME).
pub fn repos(api: &mut dyn Api, owner: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut after = Value::Null;
    loop {
        let data = query(api, owner, REPOS, json!({ "owner": owner, "after": after }))
            .map_err(|e| e.to_string())?;
        let page = &data["repositoryOwner"]["repositories"];
        let odd = || format!("{owner}: an answer that isn't a list of repos");
        for node in page["nodes"].as_array().ok_or_else(odd)? {
            out.push(node["nameWithOwner"].as_str().ok_or_else(odd)?.to_owned());
        }
        if page["pageInfo"]["hasNextPage"] != true {
            return Ok(out);
        }
        after = page["pageInfo"]["endCursor"].clone();
        if !after.is_string() {
            return Err(odd());
        }
    }
}

/// The pass: one repo when `only` names it, else every repo of each owner.
/// One repo or owner failing doesn't hold up the rest, but fails the pass.
pub fn pass(
    cfg: &Config,
    api: &mut dyn Api,
    cn: &dyn Cn,
    only: Option<&str>,
    now: &dyn Fn() -> u64,
) -> Result<(), String> {
    set_up(cfg, cn)?;
    if let Some(repo) = only {
        return sync_repo(cfg, cn, repo, now);
    }
    let mut failed = false;
    for owner in &cfg.owners {
        let listed = match repos(api, owner) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("couldn't list {owner}'s repos: {e}");
                failed = true;
                continue;
            }
        };
        for repo in listed {
            if let Err(e) = sync_repo(cfg, cn, &repo, now) {
                eprintln!("couldn't sync {repo}: {e}");
                failed = true;
            }
        }
    }
    if failed {
        Err("some repos weren't synced".to_owned())
    } else {
        Ok(())
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
