//! The drain: when the engine it hands on to has an idle slot, the oldest
//! issue triage sorted [`READY`] gets that engine's inbox label, and from
//! there it's an inbox issue like any other: its job, its answers, closed
//! when it's done. The pipeline's own repos go first, then the rest,
//! oldest first within each. At most `drain_jobs` of the
//! issues it handed on have a job at once, and one goes each pass at most;
//! every turn's end, a sorting job's included, is a pass. A handed-on job
//! waits in line like any other, never ahead of work someone asked for
//! (`first` is off for it): a `drained` mark in its issue's record says so,
//! made before the label goes on.
//!
//! Only an issue opened by one of the inbox's authors is handed on, as only
//! theirs start a job; one with a label other machinery owns, the owner's
//! label or an engine's already, is left be.

use std::fs;

use serde_json::Value;

use super::triage::READY;
use super::{Pass, RETRY, dirs, key, raw};
use crate::config::Triage;
use crate::github::api;
use crate::{Fail, io_fail, mtime, now, slot};

impl Pass<'_> {
    /// Whether the issue's record says the drain handed it on.
    pub(super) fn drained(&self, repo: &str, n: &str) -> bool {
        self.inbox
            .state
            .join(format!("{}-{n}", key(repo)))
            .join("drained")
            .exists()
    }

    /// One ready issue handed on, if there's room for it.
    pub(super) fn drain(&mut self) -> Result<(), Fail> {
        let Some(t) = self.inbox.triage.clone() else {
            return Ok(());
        };
        let Some(engine) = t.drain else {
            return Ok(());
        };
        let Some(label) = self
            .inbox
            .labels
            .iter()
            .find(|(_, e)| **e == engine)
            .map(|(l, _)| l.clone())
        else {
            return Ok(());
        };
        // Its own still going: a record with the mark and a job, or a mark
        // whose job hasn't started yet. One that hasn't after ten minutes
        // (the label didn't take, or someone took it off) is let go.
        let mut going = 0;
        for d in dirs(&self.inbox.state, "") {
            let mark = d.join("drained");
            if !mark.exists() {
                continue;
            }
            if d.join("job").exists() || mtime(&mark).is_some_and(|m| now() - m < 2 * RETRY) {
                going += 1;
            } else {
                let _ = fs::remove_dir_all(&d);
            }
        }
        if going >= t.drain_jobs || !slot::room(self.cfg, engine) {
            return Ok(());
        }
        let Some(issue) = self.next_ready(&t) else {
            return Ok(());
        };
        let repo = issue.0;
        let n = issue.1;
        let d = self.inbox.state.join(format!("{}-{n}", key(&repo)));
        fs::create_dir_all(&d).map_err(|e| io_fail(&d, e))?;
        fs::write(d.join("drained"), "").map_err(|e| io_fail(&d.join("drained"), e))?;
        if !self.add_label(&repo, &n, &label) {
            let _ = fs::remove_dir_all(&d);
            self.miss(&repo);
            return Ok(());
        }
        // Its labelled event starts the repo's pass; so does this, if that
        // never comes.
        self.book(&repo, now() + 60);
        Ok(())
    }

    /// The ready issue to hand on next, as (OWNER/REPO, N): one search for
    /// every project's owner, oldest first.
    fn next_ready(&self, t: &Triage) -> Option<(String, String)> {
        let mut owners: Vec<String> = self
            .project
            .keys()
            .filter_map(|r| r.split_once('/').map(|(o, _)| o.to_lowercase()))
            .collect();
        owners.sort();
        owners.dedup();
        if owners.is_empty() {
            return None;
        }
        let q = format!(
            "is:issue is:open label:{READY} {}",
            owners
                .iter()
                .map(|o| format!("user:{o}"))
                .collect::<Vec<_>>()
                .join(" ")
        );
        let found = api(
            self.cfg,
            &[
                "-X",
                "GET",
                "search/issues",
                "-f",
                &format!("q={q}"),
                "-f",
                "sort=created",
                "-f",
                "order=asc",
                "-f",
                "per_page=100",
            ],
        )?;
        let items = found["items"].as_array()?;
        self.pick_ready(t, items)
    }

    /// Of a search's ready issues, the one to hand on: a project's, opened by
    /// an author, nobody else's to work, and without a record here; the
    /// pipeline's repos first, then the rest, oldest first.
    pub(super) fn pick_ready(&self, t: &Triage, items: &[Value]) -> Option<(String, String)> {
        let rank = |repo: &str| {
            !self
                .cfg
                .pipeline_repos
                .iter()
                .any(|r| r.eq_ignore_ascii_case(repo))
        };
        items
            .iter()
            .filter(|i| i.get("pull_request").is_none())
            .filter(|i| self.allowed(&raw(&i["user"]["login"])) && !self.outside(i))
            .filter_map(|i| {
                let url = raw(&i["repository_url"]);
                let named = url.rsplit('/').take(2).collect::<Vec<_>>();
                let [name, owner] = named[..] else {
                    return None;
                };
                let repo = self
                    .project
                    .keys()
                    .find(|r| r.eq_ignore_ascii_case(&format!("{owner}/{name}")))?
                    .clone();
                let labels: Vec<&str> = i["labels"]
                    .as_array()
                    .map(|ls| ls.iter().filter_map(|l| l["name"].as_str()).collect())
                    .unwrap_or_default();
                let theirs = labels.iter().any(|l| {
                    self.inbox.labels.contains_key(*l)
                        || *l == t.owner_label
                        || *l == self.inbox.unstick_label
                        || t.skip.iter().any(|s| s == l)
                });
                let n = raw(&i["number"]);
                let kept = self
                    .inbox
                    .state
                    .join(format!("{}-{n}", key(&repo)))
                    .exists();
                (labels.contains(&READY) && !theirs && !kept)
                    .then(|| (rank(&repo), raw(&i["created_at"]), repo, n))
            })
            .min()
            .map(|(_, _, repo, n)| (repo, n))
    }
}
