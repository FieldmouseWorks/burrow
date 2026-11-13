//! The drain enrolls issues triage sorted [`READY`] using its configured
//! engine label. It admits the highest ranked candidate whose resolved engine has
//! room; roles may choose another engine. From there it's an inbox issue
//! like any other: its job, its answers, closed when it's done. Priority
//! goes first, then pipeline repos, then age. At most `drain_jobs` of the
//! issues it handed on have a queued or running turn at once; one goes each
//! pass at most.
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
//! for the one it picked before it labels it. What GitHub says of it then
//! replaces what the list had, so a list gone stale is put right by the
//! drain's own read, not by listing the repo again.
//!
//! The drain asks GitHub at most once a [`DRAIN_GAP`], however many passes
//! and turns' ends come in it ([`Pass::drain_once`]): one that comes sooner
//! books the drain's own pass (`proj-inbox drain`, [`DRAIN_KICK`]) for the
//! gap's end instead. A turn's end, which only frees a slot, kicks that pass
//! alone: it reads the saved lists and lists nothing.

use std::collections::BTreeSet;
use std::fs;

use serde_json::Value;

use super::triage::READY;
use super::{Pass, RETRY, dirs, key, raw};
use crate::config::Triage;
use crate::engine::Engine;
use crate::github::api;
use crate::snapshot::Priority;
use crate::{Fail, io_fail, mtime, now, slot, swap, usage};

/// In the inbox's state while a ready issue waits for a slot: the repo it's
/// in. Every turn's end then kicks the drain's own pass ([`DRAIN_KICK`],
/// `proj _ended`), since a slot freed by any job is room for it.
pub(crate) const WAITING: &str = ".drain-waiting";

/// In the inbox's state: a file for each repo (its key), its ready issues
/// as its last pass saw them.
pub(crate) const READY_LISTS: &str = ".ready";

/// What a turn's end kicks while a ready issue waits for a slot: the
/// drain's own pass (burrow-events' handler of that name runs
/// `proj-inbox drain`), which lists nothing.
pub(crate) const DRAIN_KICK: &str = "proj-inbox-drain";

/// In the inbox's state: when the drain last asked GitHub (epoch seconds).
const DRAIN_ASKED: &str = ".drain-asked";

/// Seconds between two of the drain's rounds of asking GitHub, at least.
pub(super) const DRAIN_GAP: i64 = 60;

/// A ready issue in the drain's order: its fields sort it, highest priority
/// first, then the pipeline's repos, then the oldest.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Candidate {
    pub(crate) priority: Priority,
    /// Outside the pipeline's repos: they go after its own.
    pub(crate) outside: bool,
    /// When it was opened, as GitHub says it.
    pub(crate) opened: String,
    pub(crate) repo: String,
    pub(crate) n: String,
    /// A repair child: it takes no ordinary drain capacity.
    pub(crate) repair: bool,
}

impl Pass<'_> {
    /// The drain, asking GitHub at most once a [`DRAIN_GAP`]: one that would
    /// ask sooner than that after the last books the drain's own pass for
    /// the gap's end and leaves everything as it is, its waiting mark too.
    /// One with nothing to ask about (nothing ready, nothing enrolling) is
    /// no round.
    pub(super) fn drain_once(&mut self) -> Result<(), Fail> {
        let asks = self
            .inbox
            .triage
            .as_ref()
            .is_some_and(|t| t.drain.is_some())
            && (has_ready(&self.inbox.state)
                || self.drain_records().iter().any(|d| !d.join("job").exists()));
        if asks {
            let stamp = self.inbox.state.join(DRAIN_ASKED);
            let now = now();
            if let Some(at) = crate::read(&stamp)
                .and_then(|t| t.parse::<i64>().ok())
                .filter(|at| *at <= now && now < at + DRAIN_GAP)
            {
                self.book_drain(at + DRAIN_GAP);
                return Ok(());
            }
            fs::write(&stamp, format!("{now}\n")).map_err(|e| io_fail(&stamp, e))?;
        }
        self.drain()
    }

    /// The drain's own pass is wanted then.
    pub(super) fn book_drain(&mut self, at: i64) {
        self.drain_at = Some(self.drain_at.map_or(at, |was| was.min(at)));
    }

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
    /// slot leaves [`WAITING`], and every turn's end then kicks the drain's
    /// pass (`proj _ended`); a usage hold books a pass for when it resets, and
    /// GitHub not answering about the issue picked, or not taking its label,
    /// one for five minutes on.
    pub(super) fn drain(&mut self) -> Result<(), Fail> {
        let Some(t) = self.inbox.triage.clone() else {
            return Ok(());
        };
        let Some(engine) = t.drain else {
            return Ok(());
        };
        if let Some(until) = self
            .cfg
            .settings
            .as_ref()
            .and_then(|s| s.scope_until().ok().flatten())
            && let Some(repo) = self.wake_repo()
        {
            self.book(&repo, until);
        }
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
        // Count queued/running turns in enabled repos, plus enrollments
        // whose job hasn't started yet. One that hasn't after ten minutes is
        // let go only once GitHub says the issue no longer carries the label
        // (it didn't take, someone took it off, or the issue closed): while
        // it does, the inbox is still trying to start its job, and its misses
        // tell the owner, so it holds its place rather than handing on
        // another issue that would stall the same way.
        let mut going = 0;
        for d in self.drain_records() {
            let mark = d.join("drained");
            if d.join("job").exists() {
                going += 1;
            } else if !self.still_handed(&mark, &label) {
                let _ = fs::remove_dir_all(&d);
            } else if let Some(m) = mtime(&mark).filter(|m| now() - m < 2 * RETRY) {
                going += 1;
                if let Some(repo) = self.wake_repo() {
                    self.book(&repo, m + 2 * RETRY);
                }
            } else {
                going += 1;
            }
        }
        let items = self.ready_items();
        let ranked = self.ranked(&t, &items);
        let Some(Candidate { repo: first, .. }) = ranked.first() else {
            let _ = fs::remove_file(&waiting);
            return Ok(());
        };
        // Said to be waiting before the slots are looked at: a slot freed
        // after this look has its turn's end find the mark and kick a pass,
        // and one freed before it is seen free here.
        fs::write(&waiting, format!("{first}\n")).map_err(|e| io_fail(&waiting, e))?;
        let mut blocked = false;
        // Each as it stands now, in order, and handed on. One closed,
        // sorted otherwise or handed on since its repo's pass is put right
        // in its list and passed over; one GitHub won't say about, or won't
        // take the label on, books a retry, and the rest of that repo's wait
        // for it while the next repo's go on: one repo can't hold up the
        // others. One is handed on at most.
        let mut passed: BTreeSet<String> = BTreeSet::new();
        for Candidate {
            repo, n, repair, ..
        } in ranked
        {
            // A full drain only refreshes cached repair candidates. Recheck the
            // live labels below before granting their capacity exemption.
            if (going >= t.drain_jobs && !repair) || passed.contains(&repo) {
                continue;
            }
            // No room for it as its list has it: GitHub isn't asked about
            // it, since it would wait all the same. The issue as it is now
            // is looked at again below before it's handed on.
            if !repair
                && let Some(listed) = items.iter().find(|i| {
                    raw(&i["number"]) == n
                        && raw(&i["repository_url"])
                            .to_lowercase()
                            .ends_with(&format!("/repos/{}", repo.to_lowercase()))
                })
                && let Ok(engine) = self.drain_engine(listed, &label, &repo)
                && !slot::room(self.cfg, engine)
            {
                blocked = true;
                self.no_room(&repo, engine);
                continue;
            }
            let Some(now_issue) = api(self.cfg, &format!("repos/{repo}/issues/{n}")) else {
                self.miss(&repo);
                passed.insert(repo);
                continue;
            };
            if now_issue["state"] != "open"
                || self
                    .pick_ready(&t, std::slice::from_ref(&now_issue))
                    .is_none()
            {
                // Its list had it wrong: what GitHub just said replaces it
                // there, and the next one is looked at.
                self.renote_ready(&repo, &n, &now_issue)?;
                continue;
            }
            // Repair children bypass ordinary drain capacity and slot reservation.
            // Their queued turn still enforces the selected engine's hard cap.
            let unstick = self.lane(&now_issue);
            if going >= t.drain_jobs && !unstick {
                continue;
            }
            let engine = match self.drain_engine(&now_issue, &label, &repo) {
                Ok(engine) => engine,
                Err(error) => {
                    super::say(Fail::new(1, error));
                    self.retry(&repo);
                    passed.insert(repo);
                    continue;
                }
            };
            if !unstick && !slot::room(self.cfg, engine) {
                blocked = true;
                self.no_room(&repo, engine);
                continue;
            }
            if self.cfg.paused(&repo) {
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
        if !blocked {
            let _ = fs::remove_file(&waiting);
        }
        Ok(())
    }

    /// The engine the issue's job would run on once it carries the drain's
    /// label: the exact routing the enrollment event will give start(),
    /// including a role mapped to that label.
    fn drain_engine(&self, issue: &Value, label: &str, repo: &str) -> Result<Engine, String> {
        let mut enrolled = issue.clone();
        let labelled = serde_json::json!({"name": label});
        match enrolled["labels"].as_array_mut() {
            Some(labels) => labels.push(labelled),
            None => enrolled["labels"] = Value::from(vec![labelled]),
        }
        let (role, pinned) = self.routing(&enrolled);
        crate::engine::resolve(
            self.cfg,
            role.as_deref().unwrap_or_default(),
            Some(repo),
            pinned.map(Engine::name),
        )
        .map(|choice| choice.engine)
    }

    /// No slot on the engine for the repo's ready issue: a Claude out of
    /// usage books the repo's pass for when it resets.
    fn no_room(&mut self, repo: &str, engine: Engine) {
        if engine == Engine::Claude
            && let Some(spent) = usage::out(self.cfg)
        {
            self.book(repo, spent.until);
        }
    }

    /// The records that hold one of the drain's `drain_jobs`, oldest mark
    /// first: each with its `drained` mark in an enabled repo, not a planned
    /// parent (its children hold the slots), and either enrolling or with a
    /// turn queued or running. The drain counts them, and the live snapshot
    /// shows them.
    pub(super) fn drain_records(&self) -> Vec<std::path::PathBuf> {
        let mut held: Vec<_> = dirs(&self.inbox.state, "")
            .into_iter()
            .filter(|d| self.drain_user(d).is_some())
            .collect();
        held.sort_by_key(|d| (mtime(&d.join("drained")).unwrap_or(0), d.clone()));
        held
    }

    /// Active turns and enrollments reserve capacity only in enabled repos.
    pub(super) fn drain_user(&self, dir: &std::path::Path) -> Option<String> {
        let named = crate::read(&dir.join("drained"))?;
        let (repo, n) = named.split_once('#')?;
        if !self.record_uses_capacity(dir)
            || (self.inbox.planning && self.has_task_list(Some(repo), n))
        {
            return None;
        }
        if let Some(id) = crate::read(&dir.join("job")) {
            let job = crate::job::Job::new(self.cfg, &id);
            if crate::policy::path(&job, "restoring").exists() {
                return Some(format!("{named} ({id}: restoring)"));
            }
            let state = job.state();
            matches!(state.as_str(), "queued" | "running" | "parked")
                .then(|| format!("{named} ({id}: {state})"))
        } else {
            Some(format!("{named} (enrolling)"))
        }
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
        match super::enrollment::parked(self.inbox, &issue) {
            Ok(Some(_)) => {
                if let Err(error) = super::enrollment::mark_parked(self.inbox, repo, n) {
                    eprintln!("proj-inbox: {error}");
                    self.retry(repo);
                    return true;
                }
                return false;
            }
            Err(_) => {
                self.miss(repo);
                return true;
            }
            Ok(None) => (),
        }
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
    /// for the drain: what [`Pass::pick_ready`] reads of each. A pass that
    /// looked at some issues alone changes only theirs.
    pub(super) fn note_ready(&self, repo: &str, all: &[Value]) -> Result<(), Fail> {
        let dir = self.inbox.state.join(READY_LISTS);
        let f = dir.join(key(repo));
        let others: Vec<Value> = if self.scope.is_some() {
            fs::read(&f)
                .ok()
                .and_then(|b| serde_json::from_slice::<Vec<Value>>(&b).ok())
                .unwrap_or_default()
                .into_iter()
                .filter(|i| !self.in_scope(&raw(&i["number"])))
                .collect()
        } else {
            Vec::new()
        };
        let fresh = all.iter().filter_map(ready_entry);
        save_ready(&f, others.into_iter().chain(fresh).collect())
    }

    /// Issue N of the repo's ready list as GitHub has it now: in the list
    /// while it's open and ready, out of it otherwise.
    fn renote_ready(&self, repo: &str, n: &str, now_issue: &Value) -> Result<(), Fail> {
        let f = self.inbox.state.join(READY_LISTS).join(key(repo));
        let kept = fs::read(&f)
            .ok()
            .and_then(|b| serde_json::from_slice::<Vec<Value>>(&b).ok())
            .unwrap_or_default()
            .into_iter()
            .filter(|i| raw(&i["number"]) != n);
        let open = (now_issue["state"] == "open")
            .then(|| ready_entry(now_issue))
            .flatten();
        save_ready(&f, kept.chain(open).collect())
    }

    /// The ready issues to hand on, next first, from every repo's last pass.
    /// Repair status comes from the cached labels.
    pub(super) fn next_ready(&mut self, t: &Triage) -> Vec<Candidate> {
        let items = self.ready_items();
        self.ranked(t, &items)
    }

    /// Every repo's ready list, as its last pass (or the drain) left it.
    fn ready_items(&self) -> Vec<Value> {
        let dir = self.inbox.state.join(READY_LISTS);
        fs::read_dir(&dir)
            .map(|d| {
                d.filter_map(|e| e.ok())
                    // Not one being written (swap's NAME.new).
                    .filter(|e| !e.file_name().to_string_lossy().ends_with(".new"))
                    .filter_map(|e| fs::read(e.path()).ok())
                    .filter_map(|b| serde_json::from_slice::<Vec<Value>>(&b).ok())
                    .flatten()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Of ready issues, the one to hand on ([`Pass::ranked`]'s first).
    pub(super) fn pick_ready(&mut self, t: &Triage, items: &[Value]) -> Option<(String, String)> {
        self.ranked(t, items)
            .into_iter()
            .next()
            .map(|c| (c.repo, c.n))
    }

    /// Of ready issues, the ones to hand on, in order: a project's, opened by
    /// an author, nobody else's to work, and without a record here; the
    /// priority first, then pipeline repos, then oldest first.
    fn ranked(&mut self, t: &Triage, items: &[Value]) -> Vec<Candidate> {
        let claimed: std::collections::BTreeMap<_, _> = self
            .project
            .keys()
            .map(|repo| (repo.clone(), self.claimed(repo)))
            .collect();
        let cfg = self.cfg;
        let rank = |repo: &str| {
            !cfg.pipeline_repos
                .iter()
                .any(|r| r.eq_ignore_ascii_case(repo))
        };
        items
            .iter()
            .filter(|i| i.get("pull_request").is_none())
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
                if !self.eligible(&repo, i)
                    || self.cfg.paused(&repo)
                    || super::enrollment::automatic_parked(self.inbox, &repo, i)
                {
                    return None;
                }
                let labels: Vec<&str> = i["labels"]
                    .as_array()
                    .map(|ls| ls.iter().filter_map(|l| l["name"].as_str()).collect())
                    .unwrap_or_default();
                let theirs = labels.iter().any(|l| {
                    self.inbox.labels.contains_key(*l)
                        || *l == t.owner_label
                        || t.skip.iter().any(|s| s == l)
                });
                let n = raw(&i["number"]);
                if (self.inbox.planning && self.has_task_list(Some(&repo), &n))
                    || !self.child_admissible(&repo, &n)
                {
                    return None;
                }
                let kept = self
                    .inbox
                    .state
                    .join(format!("{}-{n}", key(&repo)))
                    .exists();
                (labels.contains(&READY) && !theirs && !kept && !claimed[&repo].contains(&n)).then(
                    || Candidate {
                        priority: priority(i),
                        outside: rank(&repo),
                        opened: raw(&i["created_at"]),
                        repair: self.lane(i),
                        repo,
                        n,
                    },
                )
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
}

/// What a ready list keeps of an issue (what [`Pass::pick_ready`] reads),
/// if it's a ready one.
fn ready_entry(i: &Value) -> Option<Value> {
    let ready = i.get("pull_request").is_none()
        && i["labels"]
            .as_array()
            .is_some_and(|ls| ls.iter().any(|l| l["name"].as_str() == Some(READY)));
    ready.then(|| {
        serde_json::json!({
            "number": i["number"],
            "created_at": i["created_at"],
            "user": {"login": i["user"]["login"], "id": i["user"]["id"], "type": i["user"]["type"]},
            "labels": i["labels"],
            "repository_url": i["repository_url"],
        })
    })
}

/// A repo's ready list, written whole; none at all when it's empty.
fn save_ready(f: &std::path::Path, ready: Vec<Value>) -> Result<(), Fail> {
    if ready.is_empty() {
        let _ = fs::remove_file(f);
        return Ok(());
    }
    if let Some(dir) = f.parent() {
        fs::create_dir_all(dir).map_err(|e| io_fail(dir, e))?;
    }
    swap(f, &Value::from(ready).to_string()).map_err(|e| io_fail(f, e))
}

/// Whether any repo's ready list holds an issue.
fn has_ready(state: &std::path::Path) -> bool {
    fs::read_dir(state.join(READY_LISTS)).is_ok_and(|d| {
        d.filter_map(|e| e.ok())
            .any(|e| !e.file_name().to_string_lossy().ends_with(".new"))
    })
}

/// Highest explicit priority wins; an unlabeled issue is medium.
pub(super) fn priority(issue: &Value) -> Priority {
    priority_label(issue).map_or(Priority::Medium, |l| match l {
        "priority: high" => Priority::High,
        "priority: low" => Priority::Low,
        _ => Priority::Medium,
    })
}
pub(super) fn priority_label(issue: &Value) -> Option<&str> {
    ["priority: high", "priority: medium", "priority: low"]
        .into_iter()
        .find(|label| {
            issue["labels"]
                .as_array()
                .is_some_and(|ls| ls.iter().any(|l| l["name"].as_str() == Some(*label)))
        })
}

/// Automated inbox wakeups retain their waits while the repo is paused.
pub(crate) fn job_paused(cfg: &crate::config::Config, id: &str) -> bool {
    crate::policy::denied(&crate::job::Job::new(cfg, id))
}
