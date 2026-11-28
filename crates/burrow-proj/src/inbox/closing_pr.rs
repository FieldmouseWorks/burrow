//! Open closing-PR detection and carried-by-PR state for proj-inbox.
//!
//! Before proj-inbox launches a fresh job, starts a sorting/planning job (or
//! applies a fresh split), drains a ready issue, or offers a task-list child,
//! it asks GitHub whether an open PR already closes the issue
//! (`closedByPullRequestsReferences`). Lookups are cached per issue within a
//! single pass and fail closed when GitHub cannot be asked.
//!
//! While an open closing PR carries `OWNER/REPO#N`, its URL is kept under
//! `.carried/KEY-N` (tab-separated `OWNER/REPO#N\tPR-URL`), logged once per
//! change, and surfaced in `proj why`, `proj status`, `proj-inbox status`, and
//! `proj state --json`. Once that PR closes without closing the issue (or the
//! issue closes), the next pass clears the record and resumes normally.

use std::fs;
use std::path::PathBuf;

use serde_json::{Value, json};

use super::record::files;
use super::{Pass, key, raw};
use crate::config::Config;
use crate::{Fail, io_fail, now, swap};

/// Seconds before a repo with a carried issue is looked at again without a
/// webhook (covers cross-repo closing PRs or PR body edits).
const CLOSING_PR_LOOK: i64 = 3600;

/// GitHub GraphQL lookup for open PRs that close an issue.
const OPEN_CLOSING_QUERY: &str = "query($owner:String!,$name:String!,$n:Int!){repository(owner:$owner,name:$name){issue(number:$n){closedByPullRequestsReferences(first:100,includeClosedPrs:false){nodes{state url}}}}}";

/// Outcome of checking whether an issue already has an open closing PR.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum OpenClosingPr {
    /// GitHub confirmed no open closing PR carries this issue.
    None,
    /// This open closing PR already carries the issue.
    Open(String),
    /// GitHub could not be asked; callers must fail closed.
    Unknown,
}

impl Pass<'_> {
    /// The first open closing PR for `repo#n`, cached per `(repo, n)` for the
    /// duration of this pass. `Some(Some(url))` when one is open, `Some(None)`
    /// when none is open, `None` when GitHub could not be asked.
    pub(super) fn open_closing_pr(&mut self, repo: &str, n: u64) -> Option<Option<String>> {
        let cache_key = (repo.to_lowercase(), n);
        if let Some(cached) = self.closing_prs.get(&cache_key) {
            return cached.clone();
        }
        let fetched = self.query_open_closing_pr(repo, n);
        self.closing_prs.insert(cache_key, fetched.clone());
        fetched
    }

    fn query_open_closing_pr(&self, repo: &str, n: u64) -> Option<Option<String>> {
        let (owner, name) = repo.split_once('/')?;
        let got = crate::github::query_for(
            self.cfg,
            repo,
            OPEN_CLOSING_QUERY,
            json!({"owner": owner, "name": name, "n": n}),
        )?;
        let nodes =
            got["repository"]["issue"]["closedByPullRequestsReferences"]["nodes"].as_array()?;
        Some(
            nodes
                .iter()
                .find(|pr| pr["state"].as_str() == Some("OPEN"))
                .and_then(|pr| pr["url"].as_str())
                .filter(|u| !u.is_empty())
                .map(str::to_owned),
        )
    }

    /// Checks whether `repo#n` has an open closing PR, keeping `.carried/` in
    /// sync, logging once per change, and booking a follow-up wake-up while
    /// carried.
    pub(super) fn check_open_closing_pr(&mut self, repo: &str, n: &str) -> OpenClosingPr {
        let Ok(num) = n.parse::<u64>() else {
            return OpenClosingPr::None;
        };
        match self.open_closing_pr(repo, num) {
            Some(Some(pr)) => {
                self.note_carried(repo, n, Some(&pr));
                self.book(repo, now() + CLOSING_PR_LOOK);
                OpenClosingPr::Open(pr)
            }
            Some(None) => {
                self.note_carried(repo, n, None);
                OpenClosingPr::None
            }
            None => OpenClosingPr::Unknown,
        }
    }

    /// Persists (or clears) the carried-by-PR record for `repo#n`, logging to
    /// stderr once per state change.
    fn note_carried(&self, repo: &str, n: &str, pr: Option<&str>) {
        let k = key(repo);
        let f = files::carried_path(&self.inbox.state, &k, n);
        let was = files::carried(&self.inbox.state, &k, n);
        match pr {
            Some(pr) => {
                if was.as_deref() != Some(pr) {
                    eprintln!("proj-inbox: {repo}#{n}: carried by open closing PR {pr}");
                }
                let content = format!("{repo}#{n}\t{pr}\n");
                if let Some(dir) = f.parent()
                    && fs::create_dir_all(dir).is_ok()
                {
                    let _ = swap(&f, &content);
                }
            }
            None => {
                if let Some(prev) = was {
                    eprintln!(
                        "proj-inbox: {repo}#{n}: no longer carried by open closing PR {prev}"
                    );
                    let _ = fs::remove_file(&f);
                }
            }
        }
    }

    fn carried_records(&self) -> Vec<(String, String, PathBuf)> {
        fs::read_dir(files::carried_root(&self.inbox.state))
            .into_iter()
            .flatten()
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.ends_with(".new") {
                    return None;
                }
                let (repo, n) = name.rsplit_once('-')?;
                Some((repo.to_lowercase(), n.to_owned(), entry.path()))
            })
            .collect()
    }

    /// Retires carried-by-PR records for repositories removed from projects.
    pub(super) fn prune_carried_prs(&self) -> Result<(), Fail> {
        for (repo, _, path) in self.carried_records() {
            if !self
                .project
                .keys()
                .any(|r| key(r).eq_ignore_ascii_case(&repo))
            {
                fs::remove_file(&path).map_err(|e| io_fail(&path, e))?;
            }
        }
        Ok(())
    }

    /// Re-checks every in-scope carried issue of `repo` against GitHub so an
    /// issue unblocks immediately when its closing PR closes without merging
    /// (or drops the record when the issue is no longer open in `all`).
    pub(super) fn reconcile_carried_prs(
        &mut self,
        repo: &str,
        all: Option<&[Value]>,
    ) -> Result<(), Fail> {
        let want = key(repo).to_lowercase();
        for (kept, n, _) in self.carried_records() {
            if kept != want || !self.in_scope(&n) {
                continue;
            }
            if let Some(open) = all
                && !open.iter().any(|i| raw(&i["number"]) == n)
            {
                self.note_carried(repo, &n, None);
                continue;
            }
            if self.check_open_closing_pr(repo, &n) == OpenClosingPr::Unknown {
                self.miss(repo);
            }
        }
        Ok(())
    }

    /// Clears carried-by-PR records for in-scope issues no longer open in a
    /// freshly fetched `all` listing (e.g. closed by hand while the PR stayed
    /// open).
    pub(super) fn reconcile_carried_closed(&self, repo: &str, all: &[Value]) -> Result<(), Fail> {
        let want = key(repo).to_lowercase();
        for (kept, n, _) in self.carried_records() {
            if kept == want && self.in_scope(&n) && !all.iter().any(|i| raw(&i["number"]) == n) {
                self.note_carried(repo, &n, None);
            }
        }
        Ok(())
    }
}

/// Every issue currently recorded as carried by an open closing PR:
/// `(OWNER/REPO#N, PR-URL)`, sorted.
pub(crate) fn carried_prs(cfg: &Config) -> Vec<(String, String)> {
    let Some(inbox) = cfg.inbox.as_ref() else {
        return Vec::new();
    };
    let mut out: Vec<(String, String)> = fs::read_dir(files::carried_root(&inbox.state))
        .into_iter()
        .flatten()
        .filter_map(|e| {
            let path = e.ok()?.path();
            if path.file_name()?.to_string_lossy().ends_with(".new") {
                return None;
            }
            files::carried_entry(&path)
        })
        .collect();
    out.sort();
    out
}

/// Issues carried by an open closing PR, formatted for `Snapshot::holds.carried`
/// and `proj status`.
pub(crate) fn carried_lines(cfg: &Config) -> Vec<String> {
    carried_prs(cfg)
        .into_iter()
        .map(|(issue, pr)| format!("{issue}: carried by open closing PR {pr}"))
        .collect()
}
