//! What a pass looks at: every open issue of its repos, listed, or only
//! the issues burrow-events says its repo's events were about, each asked
//! for by number (modules/events.nix's `issueNumbers`). An issue's event
//! (opened, labelled, commented on) then costs GitHub a read of that issue,
//! not a listing of every one per label and another for triage.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use serde_json::Value;

use super::{Pass, raw, uri};
use crate::github::every;

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

    /// Whether this pass looks at issue `n`: it's in the scope, or the pass
    /// looks at every one.
    pub(super) fn in_scope(&self, n: &str) -> bool {
        self.scope
            .as_ref()
            .is_none_or(|s| n.parse().is_ok_and(|n: u64| s.contains(&n)))
    }
}
