//! Sorting questions stay with their job until a person answers or it resolves.
use super::{Pass, raw, seconds};
use crate::{Fail, ask, cmd::reply_answer, job::Job, read};
use serde_json::Value;
use std::{fs, path::Path};

impl Pass<'_> {
    pub(super) fn sorting_slot(&self, d: &Path) -> bool {
        self.holds_sorting_slot(d, true)
    }

    /// Whether a sorting record holds one of the `at_once` slots: its job's,
    /// unless its question has been left for the owner, or it has ended with
    /// no outstanding holds. `settle` first
    /// recovers the question's state under the job's lock, as a pass does;
    /// the live snapshot only reads, and takes it as it stands.
    pub(super) fn holds_sorting_slot(&self, d: &Path, settle: bool) -> bool {
        if !self.record_uses_capacity(d) {
            return false;
        }
        read(&d.join("job")).is_some_and(|id| {
            let job = Job::new(self.cfg, &id);
            let _held = if settle {
                let Ok(held) = job.lock() else { return true };
                if crate::question::recover(&job).is_err() {
                    return true;
                }
                Some(held)
            } else {
                None
            };
            // Unknown holds retain capacity. Read them once, including before
            // applying the owner-handoff exception.
            let Ok(holds) = job.holds() else { return true };
            if holds.iter().any(|h| h == "owner") {
                return false;
            }
            // A job that has ended holds a slot only for its outstanding holds.
            // Its verdict's GitHub steps are the pass's own work, so one repo
            // whose steps keep failing can't hold every repo's planners.
            // state also recognizes terminal turns without an outcome (a
            // killed unit or reboot), and keeps pending retries queued.
            matches!(job.state().as_str(), "queued" | "running" | "parked") || !holds.is_empty()
        })
    }

    /// An outstanding question is never a sorting verdict. Timed-out one-way
    /// questions retain a reply route, but no longer occupy a sorting slot.
    pub(super) fn sorting_question(
        &mut self,
        repo: &str,
        n: &str,
        d: &Path,
        job: &Job,
        open: Option<&Value>,
        busy: bool,
    ) -> Result<bool, Fail> {
        {
            let _held = job
                .lock()
                .map_err(|e| crate::io_fail(&job.path("lock"), e))?;
            if let Err(e) = crate::question::recover(job) {
                eprintln!("proj sorting question recovery: {e}");
                self.retry(repo);
                return Ok(true);
            }
        }
        let pending = ask::host_path(job, "question-needs-owner").exists();
        if ask::load(job).is_none() && !pending {
            return Ok(false);
        }
        if let Err(e) = ask::visibility(self.cfg, job) {
            eprintln!("proj sorting question: {e}");
            self.retry(repo);
        }
        let identity = read(&ask::host_path(job, "question-id"))
            .ok_or_else(|| Fail::new(1, "sorting question has no host identity"))?;
        let notice = format!("{identity}\t{}", if pending { "one-way" } else { "ask" });
        if read(&d.join("question-notice")).as_deref() != Some(notice.as_str()) {
            let question = read(&ask::host_path(job, "question-text")).unwrap_or_default();
            let body = if pending {
                format!(
                    "{question}\n\n{}",
                    job.turns()
                        .and_then(|t| read(&job.turn(&t, "result")))
                        .unwrap_or_default()
                )
            } else {
                format!(
                    "{question}{}",
                    ask::load(job).map(|q| q.notice()).unwrap_or_default()
                )
            };
            if self.post(repo, n, &body) {
                fs::write(d.join("question-notice"), &notice).map_err(|e| crate::io_fail(d, e))?;
            } else {
                self.miss(repo);
                return Ok(true);
            }
        }
        if busy {
            return Ok(true);
        }
        let Some(issue) = open else {
            return Ok(false);
        };
        let Some(written) = self.written(repo, issue) else {
            self.miss(repo);
            return Ok(true);
        };
        let Some(since) =
            read(&ask::host_path(job, "question-since")).and_then(|s| s.parse::<i64>().ok())
        else {
            return Ok(true);
        };
        let seen = read(&d.join("seen"))
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0);
        let replies: Vec<_> = written
            .comments
            .iter()
            .filter(|c| {
                c["id"].as_i64().is_some_and(|id| id > seen)
                    && self.heard(c)
                    && c["user"]["type"] != "Bot"
                    && seconds(&raw(&c["created_at"])).is_some_and(|at| at >= since)
            })
            .collect();
        if replies.is_empty() {
            return Ok(true);
        }
        let whole = self.issue_text(repo, &written.issue, &written.comments);
        let message = format!(
            "A person answered your sorting question. Read the issue as it stands, then give its final sorting verdict.\n\n{whole}"
        );
        if let Err(f) = reply_answer(self.cfg, job.id, &message, false) {
            super::say(f);
            self.retry(repo);
            return Ok(true);
        }
        Self::hand(d, &whole)?;
        let newest = written
            .comments
            .iter()
            .filter_map(|c| c["id"].as_i64())
            .max()
            .unwrap_or(seen);
        fs::write(d.join("seen"), newest.to_string()).map_err(|e| crate::io_fail(d, e))?;
        Ok(true)
    }
}
