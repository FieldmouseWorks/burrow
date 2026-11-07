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
use crate::engine::Engine;
use crate::github::api;
use crate::{Fail, io_fail, mtime, now, slot, usage};

/// In the inbox's state while a ready issue waits for a slot: the repo it's
/// in, whose pass every turn's end then kicks (`proj _ended`), since a slot
/// freed by any job is room for it.
pub(crate) const WAITING: &str = ".drain-waiting";

/// Search's pages of a hundred: a thousand results are all it gives.
const SEARCH_PAGES: usize = 10;

impl Pass<'_> {
    /// Whether the issue's record says the drain handed it on.
    pub(super) fn drained(&self, repo: &str, n: &str) -> bool {
        self.inbox
            .state
            .join(format!("{}-{n}", key(repo)))
            .join("drained")
            .exists()
    }

    /// One ready issue handed on, if there's room for it. Whatever holds it
    /// back has an event or a wake-up that brings the drain back: its own
    /// jobs' turns kick their repos' passes as they end; one waiting on a
    /// slot leaves [`WAITING`], and every turn's end then kicks a pass
    /// (`proj _ended`); a usage hold books a pass for when it resets, and a
    /// search GitHub didn't answer one for five minutes on.
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
        let waiting = self.inbox.state.join(WAITING);
        // Its own still going: a record with the mark and a job, or a mark
        // whose job hasn't started yet. One that hasn't after ten minutes
        // (the label didn't take, or someone took it off) is let go, and a
        // pass is booked for then.
        let mut going = 0;
        for d in dirs(&self.inbox.state, "") {
            let mark = d.join("drained");
            if !mark.exists() {
                continue;
            }
            if d.join("job").exists() {
                going += 1;
            } else if let Some(m) = mtime(&mark).filter(|m| now() - m < 2 * RETRY) {
                going += 1;
                if let Some(repo) = self.wake_repo() {
                    self.book(&repo, m + 2 * RETRY);
                }
            } else {
                let _ = fs::remove_dir_all(&d);
            }
        }
        if going >= t.drain_jobs {
            let _ = fs::remove_file(&waiting);
            return Ok(());
        }
        let issue = match self.next_ready(&t) {
            Ok(Some(issue)) => issue,
            Ok(None) => {
                let _ = fs::remove_file(&waiting);
                return Ok(());
            }
            Err(()) => {
                if let Some(repo) = self.wake_repo() {
                    self.retry(&repo);
                }
                return Ok(());
            }
        };
        let (repo, n) = issue;
        if !slot::room(self.cfg, engine) {
            fs::write(&waiting, format!("{repo}\n")).map_err(|e| io_fail(&waiting, e))?;
            if engine == Engine::Claude
                && let Some(spent) = usage::out(self.cfg)
            {
                self.book(&repo, spent.until);
            }
            return Ok(());
        }
        let _ = fs::remove_file(&waiting);
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

    /// The repo a pass is booked on for the drain's own sake (any pass ends
    /// with it): the pipeline's first, or else the first project.
    fn wake_repo(&self) -> Option<String> {
        self.cfg
            .pipeline_repos
            .iter()
            .find_map(|r| self.project.keys().find(|k| k.eq_ignore_ascii_case(r)))
            .or_else(|| self.project.keys().next())
            .cloned()
    }

    /// The ready issue to hand on next, as (OWNER/REPO, N), from every
    /// project owner's ready issues, each page of them (search gives a
    /// thousand at most). Err when GitHub didn't answer, which isn't the
    /// same as none being ready.
    fn next_ready(&self, t: &Triage) -> Result<Option<(String, String)>, ()> {
        let mut owners: Vec<String> = self
            .project
            .keys()
            .filter_map(|r| r.split_once('/').map(|(o, _)| o.to_lowercase()))
            .collect();
        owners.sort();
        owners.dedup();
        if owners.is_empty() {
            return Ok(None);
        }
        let q = format!(
            "is:issue is:open label:{READY} {}",
            owners
                .iter()
                .map(|o| format!("user:{o}"))
                .collect::<Vec<_>>()
                .join(" ")
        );
        let mut items = Vec::new();
        for page in 1..=SEARCH_PAGES {
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
                    "-f",
                    &format!("page={page}"),
                ],
            )
            .ok_or(())?;
            let got = found["items"].as_array().ok_or(())?;
            items.extend(got.iter().cloned());
            if got.len() < 100 {
                break;
            }
        }
        Ok(self.pick_ready(t, &items))
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
            .filter(|i| self.eligible(i))
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
