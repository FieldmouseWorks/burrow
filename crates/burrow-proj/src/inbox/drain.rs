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
        for d in dirs(&self.inbox.state, "") {
            let mark = d.join("drained");
            if !self.draining_record(&d) {
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
        let ranked = self.next_ready(&t);
        let Some((first, _, _)) = ranked.first() else {
            let _ = fs::remove_file(&waiting);
            return Ok(());
        };
        // Said to be waiting before the slots are looked at: a slot freed
        // after this look has its turn's end find the mark and kick a pass,
        // and one freed before it is seen free here.
        fs::write(&waiting, format!("{first}\n")).map_err(|e| io_fail(&waiting, e))?;
        let mut blocked = false;
        // Each as it stands now, in order, and handed on. One closed,
        // sorted otherwise or handed on since its repo's pass is passed
        // over, and that pass, booked now, lists afresh; one GitHub won't
        // say about, or won't take the label on, books a retry. Either way
        // the rest of that repo's wait for its pass, and the next repo's go
        // on: one repo can't hold up the others. One is handed on at most.
        let mut passed: BTreeSet<String> = BTreeSet::new();
        for (repo, n, repair) in ranked {
            // A full drain only refreshes cached repair candidates. Recheck the
            // live labels below before granting their capacity exemption.
            if (going >= t.drain_jobs && !repair) || passed.contains(&repo) {
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
                self.book(&repo, now());
                passed.insert(repo);
                continue;
            }
            // Repair children bypass ordinary drain capacity and slot reservation.
            // Their queued turn still enforces the selected engine's hard cap.
            let unstick = self.lane(&now_issue);
            if going >= t.drain_jobs && !unstick {
                continue;
            }
            // Resolve the exact routing metadata the enrollment event will
            // give start(), including a role mapped to the enrollment label.
            let mut enrolled = now_issue;
            enrolled["labels"]
                .as_array_mut()
                .expect("ready issue labels")
                .push(serde_json::json!({"name": label}));
            let (role, pinned) = self.routing(&enrolled);
            let engine = match crate::engine::resolve(
                self.cfg,
                role.as_deref().unwrap_or_default(),
                Some(&repo),
                pinned.map(Engine::name),
            ) {
                Ok(choice) => choice.engine,
                Err(error) => {
                    super::say(Fail::new(1, error));
                    self.retry(&repo);
                    passed.insert(repo);
                    continue;
                }
            };
            if !unstick && !slot::room(self.cfg, engine) {
                blocked = true;
                if engine == Engine::Claude
                    && let Some(spent) = usage::out(self.cfg)
                {
                    self.book(&repo, spent.until);
                }
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
                    "user": {"login": i["user"]["login"], "id": i["user"]["id"], "type": i["user"]["type"]},
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

    /// The ready issues to hand on, next first, as (OWNER/REPO, N, repair),
    /// from every repo's last pass. Repair status comes from the cached labels.
    fn next_ready(&mut self, t: &Triage) -> Vec<(String, String, bool)> {
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

    pub(super) fn ready_queue(&mut self, t: &Triage) -> String {
        let users: Vec<_> = dirs(&self.inbox.state, "")
            .iter()
            .filter_map(|dir| self.drain_user(dir))
            .collect();
        let mut output = format!("drain: {}/{} used\n", users.len(), t.drain_jobs);
        for user in users {
            output.push_str(&format!("  {user}\n"));
        }
        output.push_str(
            &self
                .next_ready(t)
                .into_iter()
                .take(5)
                .enumerate()
                .map(|(index, (repo, n, _))| format!("ready {}: {repo}#{n}\n", index + 1))
                .collect::<String>(),
        );
        output
    }

    /// Of ready issues, the one to hand on ([`Pass::ranked`]'s first).
    pub(super) fn pick_ready(&mut self, t: &Triage, items: &[Value]) -> Option<(String, String)> {
        self.ranked(t, items)
            .into_iter()
            .next()
            .map(|(repo, n, _)| (repo, n))
    }

    /// Of ready issues, the ones to hand on, in order: a project's, opened by
    /// an author, nobody else's to work, and without a record here; the
    /// priority first, then pipeline repos, then oldest first.
    fn ranked(&mut self, t: &Triage, items: &[Value]) -> Vec<(String, String, bool)> {
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
                if !self.eligible(&repo, i) || self.cfg.paused(&repo) {
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
                    || {
                        (
                            priority(i),
                            rank(&repo),
                            raw(&i["created_at"]),
                            repo,
                            n,
                            self.lane(i),
                        )
                    },
                )
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|(_, _, _, repo, n, repair)| (repo, n, repair))
            .collect()
    }
}

/// Highest explicit priority wins; an unlabeled issue is medium.
pub(super) fn priority(issue: &Value) -> u8 {
    priority_label(issue).map_or(1, |l| match l {
        "priority: high" => 0,
        "priority: low" => 2,
        _ => 1,
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

/// The next candidates from the drain's own lists and ranking.
pub(crate) fn ready_queue(cfg: &crate::config::Config) -> String {
    let Some(inbox) = cfg.inbox.as_ref() else {
        return String::new();
    };
    let Some(t) = inbox.triage.as_ref().filter(|t| t.drain.is_some()) else {
        return String::new();
    };
    let mut pass = Pass {
        cfg,
        inbox,
        project: super::projects(cfg),
        wake: std::collections::BTreeMap::new(),
        unreached: Vec::new(),
        looked: Vec::new(),
    };
    pass.ready_queue(t)
}
