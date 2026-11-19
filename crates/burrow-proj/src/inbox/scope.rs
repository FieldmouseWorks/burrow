//! What a pass looks at: every open issue of its repos, listed, or only
//! the issues burrow-events says its repo's events were about, each asked
//! for by number (modules/events.nix's `issueNumbers`). An issue's event
//! (opened, labelled, commented on) then costs GitHub a read of that issue,
//! not a listing of every one per label and another for triage.
//!
//! The listing of every open issue a repo has, which triage and the drain
//! read, is the costly one: a repo's passes make it at most once a
//! [`LISTING_GAP`], however many kicks and wake-ups come in it. A pass that
//! comes sooner books the repo's pass for the gap's end, which lists.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use serde_json::Value;

use super::{Pass, key, raw, uri};
use crate::github::every;
use crate::{Fail, io_fail, now};

/// In the inbox's state: a file for each repo (its key), when its passes
/// last listed every open issue it has (epoch seconds).
pub(super) const LISTED: &str = ".listed";

/// Seconds between two listings of every open issue of a repo, at least.
pub(super) const LISTING_GAP: i64 = 120;

/// The issues burrow-events says this repo's pass is for (the file
/// BURROW_EVENTS_ISSUES names holds their numbers, one a line, while it
/// runs): an issue's event's pass looks at those alone. None, for a look at
/// every issue, when it says none (a kick, a wake-up, a hand run) or says
/// something else.
pub(super) fn told() -> Option<BTreeSet<u64>> {
    let f = std::env::var_os("BURROW_EVENTS_ISSUES")?;
    issues_in(&fs::read_to_string(f).ok()?)
}

pub(super) fn issues_in(text: &str) -> Option<BTreeSet<u64>> {
    let ns = text
        .split_whitespace()
        .map(str::parse)
        .collect::<Result<BTreeSet<u64>, _>>()
        .ok()?;
    (!ns.is_empty()).then_some(ns)
}

impl Pass<'_> {
    /// The repo's open issues labelled for the inbox, one listing a label;
    /// None when GitHub can't be asked.
    pub(super) fn labelled(&self, repo: &str) -> Option<Vec<Value>> {
        let mut issues = BTreeMap::new();
        for label in self.inbox.labels.keys() {
            let open = every(
                self.cfg,
                &format!(
                    "repos/{repo}/issues?labels={}&state=open&per_page=100",
                    uri(label)
                ),
            )?;
            for issue in open {
                issues.insert(raw(&issue["number"]), issue);
            }
        }
        Some(
            issues
                .into_values()
                .filter(|i| i.get("pull_request").is_none())
                .collect(),
        )
    }

    /// Of these issues of the repo, and the split parents any of them is a
    /// child of (whose list moves on as a child closes), the open ones, each
    /// asked for by number; the scope grows by the parents. None when
    /// GitHub can't be asked.
    pub(super) fn asked(&mut self, repo: &str, mut ns: BTreeSet<u64>) -> Option<Vec<Value>> {
        ns.extend(self.parents_of(repo, &ns));
        self.scope = Some(ns.clone());
        let mut open = Vec::new();
        for n in ns {
            let issue = crate::github::api(self.cfg, &format!("repos/{repo}/issues/{n}"))?;
            // gh follows transfers. A redirected response must never be
            // acted on under this repo's identity, even when its number is
            // unchanged. Discard the whole read before any effects; run()
            // records a miss and books the ordinary, full reconciliation.
            if issue["number"].as_u64() != Some(n)
                || !issue["repository_url"].as_str().is_some_and(|url| {
                    url.eq_ignore_ascii_case(&format!("https://api.github.com/repos/{repo}"))
                })
            {
                return None;
            }
            if issue["state"] == "open" {
                open.push(issue);
            }
        }
        Some(open)
    }

    /// When the repo may be listed again, if a listing within the last
    /// [`LISTING_GAP`] means it may not be now.
    pub(super) fn listing_due(&self, repo: &str) -> Option<i64> {
        let now = now();
        crate::read(&self.inbox.state.join(LISTED).join(key(repo)))
            .and_then(|t| t.parse::<i64>().ok())
            .filter(|at| *at <= now && now < at + LISTING_GAP)
            .map(|at| at + LISTING_GAP)
    }

    /// Every open issue of the repo, listed; None when GitHub can't be
    /// asked. The attempt's time is kept for [`Pass::listing_due`] before
    /// the first page is asked for, so one that fails, on any page, holds
    /// the next off for the gap as well.
    pub(super) fn list_all(&self, repo: &str) -> Result<Option<Vec<Value>>, Fail> {
        let dir = self.inbox.state.join(LISTED);
        fs::create_dir_all(&dir).map_err(|e| io_fail(&dir, e))?;
        let f = dir.join(key(repo));
        fs::write(&f, format!("{}\n", now())).map_err(|e| io_fail(&f, e))?;
        Ok(every(
            self.cfg,
            &format!("repos/{repo}/issues?state=open&per_page=100"),
        ))
    }

    /// Whether this pass looks at issue `n`: it's in the scope, or the pass
    /// looks at every one.
    pub(super) fn in_scope(&self, n: &str) -> bool {
        self.scope
            .as_ref()
            .is_none_or(|s| n.parse().is_ok_and(|n: u64| s.contains(&n)))
    }
}
