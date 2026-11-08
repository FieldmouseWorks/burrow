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
//!
//! What's ready comes from the passes' own lists of each repo's open
//! issues, every page of them: each repo's pass keeps its ready ones in
//! [`READY_LISTS`], and the drain picks from all of them, then asks GitHub
//! for the one it picked before it labels it.

use std::collections::BTreeSet;
use std::fs;

use serde_json::Value;

use super::triage::READY;
use super::{Pass, RETRY, dirs, key, raw};
use crate::config::Triage;
use crate::engine::Engine;
use crate::github::api;
use crate::{Fail, io_fail, mtime, now, slot, swap, usage};

/// In the inbox's state while a ready issue waits for a slot: the repo it's
/// in, whose pass every turn's end then kicks (`proj _ended`), since a slot
/// freed by any job is room for it.
pub(crate) const WAITING: &str = ".drain-waiting";

/// In the inbox's state: a file for each repo (its key), its ready issues
/// as its last pass saw them.
pub(crate) const READY_LISTS: &str = ".ready";

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
    /// (`proj _ended`); a usage hold books a pass for when it resets, and
    /// GitHub not answering about the issue picked, or not taking its label,
    /// one for five minutes on.
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
        // whose job hasn't started yet. One that hasn't after ten minutes is
        // let go only once GitHub says the issue no longer carries the label
        // (it didn't take, someone took it off, or the issue closed): while
        // it does, the inbox is still trying to start its job, and its misses
        // tell the owner, so it holds its place rather than handing on
        // another issue that would stall the same way.
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
            } else if self.still_handed(&mark, &label) {
                going += 1;
            } else {
                let _ = fs::remove_dir_all(&d);
            }
        }
        if going >= t.drain_jobs {
            let _ = fs::remove_file(&waiting);
            return Ok(());
        }
        let ranked = self.next_ready(&t);
        let Some((first, _)) = ranked.first() else {
            let _ = fs::remove_file(&waiting);
            return Ok(());
        };
        // Said to be waiting before the slots are looked at: a slot freed
        // after this look has its turn's end find the mark and kick a pass,
        // and one freed before it is seen free here.
        fs::write(&waiting, format!("{first}\n")).map_err(|e| io_fail(&waiting, e))?;
        if !slot::room(self.cfg, engine) {
            if engine == Engine::Claude
                && let Some(spent) = usage::out(self.cfg)
            {
                self.book(first, spent.until);
            }
            return Ok(());
        }
        let _ = fs::remove_file(&waiting);
        // Each as it stands now, in order, and handed on. One closed,
        // sorted otherwise or handed on since its repo's pass is passed
        // over, and that pass, booked now, lists afresh; one GitHub won't
        // say about, or won't take the label on, books a retry. Either way
        // the rest of that repo's wait for its pass, and the next repo's go
        // on: one repo can't hold up the others. One is handed on at most.
        let mut passed: BTreeSet<String> = BTreeSet::new();
        for (repo, n) in ranked {
            if passed.contains(&repo) {
                continue;
            }
            let Some(now_issue) = api(self.cfg, &format!("repos/{repo}/issues/{n}")) else {
                self.miss(&repo);
                passed.insert(repo);
                continue;
            };
            if now_issue["state"] != "open" || self.pick_ready(&t, &[now_issue]).is_none() {
                self.book(&repo, now());
                passed.insert(repo);
                continue;
            }
            let d = self.inbox.state.join(format!("{}-{n}", key(&repo)));
            fs::create_dir_all(&d).map_err(|e| io_fail(&d, e))?;
            fs::write(d.join("drained"), format!("{repo}#{n}\n"))
                .map_err(|e| io_fail(&d.join("drained"), e))?;
            if !self.add_label(&repo, &n, &label) {
                let _ = fs::remove_dir_all(&d);
                self.miss(&repo);
                passed.insert(repo);
                continue;
            }
            // Its labelled event starts the repo's pass; so does this, if
            // that never comes.
            self.book(&repo, now() + 60);
            break;
        }
        Ok(())
    }

    /// Whether the issue a `drained` mark names (OWNER/REPO#N) is open with
    /// the engine's label still on, as GitHub has it now. One GitHub won't
    /// say about counts as still handed on, its repo's retry booked; a mark
    /// that names none is let go.
    fn still_handed(&mut self, mark: &std::path::Path, label: &str) -> bool {
        let named = fs::read_to_string(mark).unwrap_or_default();
        let Some((repo, n)) = named.trim().split_once('#') else {
            return false;
        };
        let Some(issue) = api(self.cfg, &format!("repos/{repo}/issues/{n}")) else {
            self.miss(repo);
            return true;
        };
        issue["state"] == "open"
            && issue["labels"]
                .as_array()
                .is_some_and(|ls| ls.iter().any(|l| l["name"].as_str() == Some(label)))
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

    /// A repo's ready issues, from its pass's list of every open one, kept
    /// for the drain: what [`Pass::pick_ready`] reads of each.
    pub(super) fn note_ready(&self, repo: &str, all: &[Value]) -> Result<(), Fail> {
        let dir = self.inbox.state.join(READY_LISTS);
        let f = dir.join(key(repo));
        let ready: Vec<Value> = all
            .iter()
            .filter(|i| i.get("pull_request").is_none())
            .filter(|i| {
                i["labels"]
                    .as_array()
                    .is_some_and(|ls| ls.iter().any(|l| l["name"].as_str() == Some(READY)))
            })
            .map(|i| {
                serde_json::json!({
                    "number": i["number"],
                    "created_at": i["created_at"],
                    "user": {"login": i["user"]["login"]},
                    "labels": i["labels"],
                    "repository_url": i["repository_url"],
                })
            })
            .collect();
        if ready.is_empty() {
            let _ = fs::remove_file(&f);
            return Ok(());
        }
        fs::create_dir_all(&dir).map_err(|e| io_fail(&dir, e))?;
        swap(&f, &Value::from(ready).to_string()).map_err(|e| io_fail(&f, e))
    }

    /// The ready issues to hand on, next first, as (OWNER/REPO, N), of every
    /// repo's as its last pass listed them.
    fn next_ready(&self, t: &Triage) -> Vec<(String, String)> {
        let dir = self.inbox.state.join(READY_LISTS);
        let items: Vec<Value> = fs::read_dir(&dir)
            .map(|d| {
                d.filter_map(|e| e.ok())
                    // Not one being written (swap's NAME.new).
                    .filter(|e| !e.file_name().to_string_lossy().ends_with(".new"))
                    .filter_map(|e| fs::read(e.path()).ok())
                    .filter_map(|b| serde_json::from_slice::<Vec<Value>>(&b).ok())
                    .flatten()
                    .collect()
            })
            .unwrap_or_default();
        self.ranked(t, &items)
    }

    /// Of ready issues, the one to hand on ([`Pass::ranked`]'s first).
    pub(super) fn pick_ready(&self, t: &Triage, items: &[Value]) -> Option<(String, String)> {
        self.ranked(t, items).into_iter().next()
    }

    /// Of ready issues, the ones to hand on, in order: a project's, opened by
    /// an author, nobody else's to work, and without a record here; the
    /// pipeline's repos first, then the rest, oldest first.
    fn ranked(&self, t: &Triage, items: &[Value]) -> Vec<(String, String)> {
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
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|(_, _, repo, n)| (repo, n))
            .collect()
    }
}
