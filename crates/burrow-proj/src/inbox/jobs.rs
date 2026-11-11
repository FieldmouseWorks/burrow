//! A pass's look at the jobs it already has for a repo's issues: each turn's
//! answer posted, a cut-off turn carried on, the job ended with its issue,
//! and replies handed on.

use std::fs;
use std::path::Path;

use serde_json::Value;

use super::{CUT_OFF, Pass, dirs, key, raw, say, seconds, trimmed};
use crate::cmd::{halt, reply, reply_answer, stop, told, unstick};
use crate::github::api;
use crate::job::Job;
use crate::{Fail, filled, io_fail, now, read};

impl Pass<'_> {
    /// Jobs already running for this repo's issues.
    pub(super) fn jobs(&mut self, repo: &str, p: &str, open: &[Value]) -> Result<(), Fail> {
        // A job carries on only while an agent may still read its issue: one
        // since labelled outside text, or whose opener is no longer one of
        // the authors, is stopped at once, queued or running, and its record
        // goes, the next look at the issue (new_issues) saying why.
        self.retired(repo)?;
        let listed = open;
        let open: Vec<Value> = open.iter().filter(|i| self.eligible(i)).cloned().collect();
        let open = open.as_slice();
        'jobs: for d in dirs(&self.inbox.state, &format!("{}-", key(repo))) {
            let Some(mut id) = read(&d.join("job")).filter(|j| !j.is_empty()) else {
                continue;
            };
            let issue = read(&d.join("issue")).unwrap_or_default();
            let Some((of, n)) = issue.rsplit_once('#') else {
                continue;
            };
            if of != repo {
                continue;
            }
            if d.join("unannounced").exists() {
                if self.post(repo, n, &format!("On it: `proj` job `{id}` in {p}.")) {
                    let _ = fs::remove_file(d.join("unannounced"));
                } else {
                    self.miss(repo);
                }
            }
            // A job dir removed by hand: that job is over. The issue's record
            // goes with it, and one still open is taken up afresh next pass.
            let job = Job::new(self.cfg, &id);
            if !job.dir.is_dir() {
                let _ = fs::remove_dir_all(&d);
                continue;
            }
            {
                let _held = job.lock().map_err(|e| io_fail(&job.path("lock"), e))?;
                if let Err(e) = crate::question::recover(&job) {
                    eprintln!("proj question recovery: {e}");
                    self.retry(repo);
                    continue;
                }
            }
            // Adopt existing machine issues too: a deployment must not leave
            // the long-lived stuck job pinned to its old label's engine.
            if let Some(role) = open
                .iter()
                .find(|i| raw(&i["number"]) == n)
                .and_then(|i| self.machine_role(i))
            {
                let _held = job.lock().map_err(|e| io_fail(&job.path("lock"), e))?;
                crate::swap(&job.path("role"), &format!("{role}\n"))
                    .map_err(|e| io_fail(&job.path("role"), e))?;
                match fs::remove_file(job.path("engine")) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(io_fail(&job.path("engine"), e)),
                }
            }
            // The state and the turn count, read together: a turn starts
            // (proj reply, proj _watch) holding the job's lock, and halfway
            // through it the count names the new turn while the state is
            // still the last one's.
            let (st, turns) = {
                let _held = job.read_lock().map_err(|e| io_fail(&job.path("lock"), e))?;
                let turns: i64 = job.turns().and_then(|t| t.parse().ok()).unwrap_or(0);
                (job.state(), turns)
            };
            let busy = matches!(st.as_str(), "queued" | "running");
            // Barred, as the list has it; or, for a turn still to finish whose
            // issue the list hasn't (closed, or its label taken off), as
            // GitHub says now: closing it lets the turn finish only while
            // its issue may still be read.
            let mine = listed.iter().find(|i| raw(&i["number"]) == n);
            let split_pending = crate::planning::admit(&job).is_err();
            if (mine.is_some() || busy) && {
                let Some(barred) = self.barred(repo, n, mine) else {
                    self.miss(repo);
                    continue;
                };
                barred
            } {
                if let Err(f) = stop(self.cfg, &id, false) {
                    say(f);
                    self.retry(repo);
                    continue;
                }
                let _ = fs::remove_dir_all(&d);
                continue;
            }

            // Every turn that has finished gets its answer on the issue, in
            // order: all but the one queued or running now. A job that proj
            // _watch resumed before this pass still has the answer it gave
            // before posted.
            let last = if busy { turns - 1 } else { turns };
            let mut k: i64 = read(&d.join("answered"))
                .and_then(|a| a.parse().ok())
                .unwrap_or(0);
            // The latest turn, cut off, carries on in its own conversation
            // while the issue is open, twice in a row at most (proj says
            // whether it was, under the lock it starts the next turn under),
            // and the issue hears so. In a row: the turn the last carrying on
            // started (revived has it, and how many in a row led to it) is
            // the one cut off now.
            let now_open = open.iter().find(|i| raw(&i["number"]) == n);
            let mut rv = revived(&d);
            let streak = if rv.0 == Some(turns) { rv.1 } else { 0 };
            let mut revive = false;
            if !split_pending && st == "failed" && k < turns && streak < 2 && now_open.is_some() {
                match reply(self.cfg, &id, CUT_OFF, true, Some(&turns.to_string())) {
                    Ok(()) => {
                        revive = true;
                        rv = (Some(turns + 1), streak + 1);
                        fs::write(d.join("revived"), format!("{} {}\n", turns + 1, streak + 1))
                            .map_err(|e| io_fail(&d.join("revived"), e))?;
                    }
                    // Not cut off: its answer, or what it ended on, is posted.
                    Err(f) if f.code == 3 => say(f),
                    // Cut off, but its next turn couldn't start (proj said
                    // why): neither posted nor counted, it's tried again in
                    // five minutes.
                    Err(f) => {
                        say(f);
                        self.retry(repo);
                        continue;
                    }
                }
            }
            while k < last {
                k += 1;
                let why = if k == turns {
                    format!(" ({st})")
                } else {
                    String::new()
                };
                let body = if rv.0 == Some(k + 1) {
                    format!(
                        "The job's turn {k} was cut off before it answered (its whole unit was killed, out of memory as a rule): it carries on where it left off."
                    )
                } else {
                    told(self.cfg, &id, &k.to_string())
                        .map(|t| trimmed(&t))
                        .unwrap_or_else(|_| {
                            format!(
                                "The job's turn {k} ended without an answer{why}. `proj log {id} --turn {k} --raw` on {} has what it did.",
                                self.inbox.host
                            )
                        })
                };
                // GitHub takes 65536 characters a comment.
                let engine = job.engine_label(&k.to_string());
                let body: String = format!("**{engine} job**\n\n{body}")
                    .chars()
                    .take(60000)
                    .collect();
                if !self.post(repo, n, &body) {
                    self.miss(repo);
                    continue 'jobs;
                }
                fs::write(d.join("answered"), format!("{k}\n"))
                    .map_err(|e| io_fail(&d.join("answered"), e))?;
            }
            if let Err(error) = crate::ask::visibility(self.cfg, &job) {
                eprintln!("proj question: {error}");
                self.retry(repo);
            }
            let needs_owner = crate::ask::host_path(&job, "question-needs-owner").exists();
            // Labelled to wait in no line: the job is made one that doesn't
            // (proj unstick), whatever it's doing, and again every pass. That
            // tells the watchdog afresh, since a turn begun before the switch
            // that brought proj unstick writes the job's waits without it, and
            // its own stall would go back on the issue it's the job of. A turn
            // of it still in line leaves it at its next look, every later turn
            // takes that slot, and one running carries on.
            if now_open.is_some_and(|i| self.lane(i))
                && let Err(f) = unstick(self.cfg, &id)
            {
                say(f);
                self.retry(repo);
                continue;
            }
            if busy {
                continue;
            }
            if self.inbox.planning
                && let Some(split_turn) = crate::planning::pending_split(&job)
                && let Some(parent) = now_open
            {
                {
                    let _held = job.lock().map_err(|e| io_fail(&job.path("lock"), e))?;
                    if job.turns().as_deref() != Some(&turns.to_string())
                        || matches!(job.state().as_str(), "queued" | "running")
                    {
                        self.retry(repo);
                        continue;
                    }
                    fs::write(job.path("planning-hold"), "finishing the task split\n")
                        .map_err(|e| io_fail(&job.path("planning-hold"), e))?;
                }
                let Some(written) = self.written(repo, parent) else {
                    self.miss(repo);
                    continue;
                };
                let input = self.issue_text(repo, &written.issue, &written.comments);
                if !Self::handed(&d, &input) {
                    // Explicitly invalidate the proposal, preserving its data and
                    // all partial creation receipts. Normal reply delivery below
                    // will hand the updated verified task to the worker next pass.
                    let _held = job.lock().map_err(|e| io_fail(&job.path("lock"), e))?;
                    if job.turns().as_deref() != Some(&turns.to_string())
                        || matches!(job.state().as_str(), "queued" | "running")
                    {
                        self.retry(repo);
                        continue;
                    }
                    // Intent still bars admission if we crash before the rename.
                    match fs::remove_file(job.path("planning-hold")) {
                        Ok(()) => (),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                        Err(e) => return Err(io_fail(&job.path("planning-hold"), e)),
                    }
                    fs::rename(
                        job.turn(&split_turn, "split"),
                        job.turn(&split_turn, "split-invalid"),
                    )
                    .map_err(|e| io_fail(&job.turn(&split_turn, "split"), e))?;
                    self.retry(repo);
                    continue;
                }
                if !self.begin_worker_split(repo, &written.issue) {
                    self.miss(repo);
                    continue;
                }
                let tasks = read(&job.turn(&split_turn, "split"))
                    .and_then(|text| crate::planning::parse(&text).ok());
                if tasks.as_ref().is_none_or(|tasks| {
                    !self.prepare_plan(repo, n, &input, tasks)
                        || !self.split_children(repo, n, tasks)
                }) || !self.add_label(repo, n, "tracker")
                {
                    self.miss(repo);
                    continue;
                }
                // Children no longer need their parent's worker or drain slot.
                // Keep the plan/receipts independently of the retired record.
                let root = self
                    .inbox
                    .state
                    .join(".retired")
                    .join(key(repo).to_lowercase())
                    .join(n);
                fs::create_dir_all(&root).map_err(|e| io_fail(&root, e))?;
                let retired = root.join(&id);
                fs::rename(&d, &retired).map_err(|e| io_fail(&retired, e))?;
                self.retire(repo, n, &id, &retired)?;
                continue;
            }
            if split_pending && now_open.is_some() {
                continue;
            }

            // Closed, or the label taken off: done with it, and so is its
            // job, which nothing it awaits brings back. A job that couldn't
            // be stopped (proj stop says why) is tried again in five minutes;
            // the rest are still seen to.
            let Some(listed_now) = now_open else {
                let root = self
                    .inbox
                    .state
                    .join(".retired")
                    .join(key(repo).to_lowercase())
                    .join(n);
                fs::create_dir_all(&root).map_err(|e| io_fail(&root, e))?;
                let retired = root.join(&id);
                fs::rename(&d, &retired).map_err(|e| io_fail(&retired, e))?;
                self.retire(repo, n, &id, &retired)?;
                continue;
            };

            // Carried on: replies wait for the next pass, which its turn's end
            // kicks.
            if revive {
                continue;
            }

            // Replies since the last turn continue the job. Only once they're
            // a minute old: closing with a comment posts the comment first,
            // and the issue list can show an issue open for a few seconds
            // after it closed. So the issue is asked for again too, before
            // the reply is handed on.
            let seen: i64 = read(&d.join("seen"))
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let Some(written) = self.written(repo, listed_now) else {
                self.miss(repo);
                continue;
            };
            let safe_issue = &written.issue;
            let all = &written.comments;
            let listed_now = safe_issue;
            let before = jiff::Timestamp::from_second(now() - 60)
                .map(|t| t.strftime("%Y-%m-%dT%H:%M:%SZ").to_string())
                .unwrap_or_default();
            let made = |c: &Value| c["created_at"].as_str().unwrap_or_default().to_owned();
            let after = |c: &Value| c["id"].as_i64().is_some_and(|i| i > seen);
            // What the job has had (whatever their age: a job's first turn
            // is handed every comment there is), and the replies since that
            // are a minute old.
            let comments: Vec<Value> = all
                .iter()
                .filter(|c| !after(c) || made(c) < before)
                .cloned()
                .collect();
            // A newer reply is handed on once it's a minute old: one of the
            // authors', never the inbox's own answer.
            let young = all
                .iter()
                .filter(|c| after(c) && made(c) >= before && self.heard(c))
                .filter_map(|c| seconds(&made(c)))
                .min();
            if let Some(young) = young {
                self.book(repo, young + 61);
            }
            let msg: Vec<String> = comments
                .iter()
                .filter(|c| after(c) && self.heard(c))
                .map(|c| {
                    format!(
                        "{} replied on the issue:\n\n{}",
                        raw(&c["user"]["login"]),
                        raw(&c["body"])
                    )
                })
                .collect();
            let msg = trimmed(&msg.join("\n\n"));
            let newest = comments
                .iter()
                .filter_map(|c| c["id"].as_i64())
                .max()
                .unwrap_or(seen);
            // What the job has had of the issue: its title and text, and the
            // authors' comments up to the last it was handed.
            let had: Vec<Value> = all.iter().filter(|c| !after(c)).cloned().collect();
            // Finished (its last turn ran to its end and said so with `proj
            // done`, not `ask` or `await`), every answer posted and no reply
            // since: the issue may be closed with it, so what's open is what's
            // still to do. The stuck issue isn't the job's to close: the
            // steward closes it once nothing is stuck.
            let closable = !needs_owner
                && msg.is_empty()
                && young.is_none()
                && k >= turns
                && st == "done"
                && read(&job.turn(&turns.to_string(), "end")).as_deref() == Some("done")
                && !self.lane(listed_now)
                && self.child_can_close(repo, n);
            let listed_changed = !Self::handed(&d, &self.issue_text(repo, listed_now, &had));
            if closable
                || !msg.is_empty()
                || listed_changed
                || job.turn(&turns.to_string(), "split-invalid").exists()
            {
                // Decided on the issue as GitHub has it now, not as the list
                // read before this pass started jobs had it.
                let Some(now_issue) = api(self.cfg, &format!("repos/{repo}/issues/{n}")) else {
                    self.miss(repo);
                    continue;
                };
                // A later REST read may have observed an edit after our verified
                // snapshot. Never overwrite that signal and close stale work.
                if !written.matches(&now_issue) {
                    self.retry(repo);
                    continue;
                }
                let mut now_issue = now_issue;
                now_issue["title"] = safe_issue["title"].clone();
                now_issue["body"] = safe_issue["body"].clone();
                let labelled = self.engine(&now_issue).is_some();
                if now_issue["state"] != "open" || !labelled || !self.eligible(&now_issue) {
                    continue;
                }
                // Not what the job's record says it was handed (the title or
                // text edited, a comment it had edited or deleted, or a
                // record from before what it was handed was kept): changed,
                // and the job is handed the issue as it stands, replies and
                // all, rather than the issue closed.
                let changed = !Self::handed(&d, &self.issue_text(repo, &now_issue, &had))
                    || job.turn(&turns.to_string(), "split-invalid").exists();
                // Closed with the job, and the job stopped, under the lock
                // turns start under: a reply (proj reply, proj _watch) that
                // started a turn since the look above leaves the issue open,
                // and none starts between the close and the stop. Reopening
                // it starts a job afresh from the issue as it stands.
                if closable && !changed {
                    let held = job.lock().map_err(|e| io_fail(&job.path("lock"), e))?;
                    let still: i64 = job.turns().and_then(|t| t.parse().ok()).unwrap_or(0);
                    if still == turns
                        && job.state() == "done"
                        && read(&job.turn(&turns.to_string(), "end")).as_deref() == Some("done")
                    {
                        if !self.close(repo, n, "completed") {
                            drop(held);
                            self.miss(repo);
                            continue;
                        }
                        if let Err(f) = halt(self.cfg, &job, false) {
                            drop(held);
                            say(f);
                            self.retry(repo);
                            continue;
                        }
                        drop(held);
                        let _ = fs::remove_dir_all(&d);
                        continue;
                    }
                }
                let whole = self.issue_text(repo, &now_issue, &comments);
                let human_reply = comments.iter().any(|c| {
                    after(c)
                        && self.heard(c)
                        && c["user"]["type"] != "Bot"
                        && !raw(&c["user"]["login"]).ends_with("[bot]")
                });
                let msg = if changed {
                    format!(
                        "The issue has changed since you were handed it: its title or text edited, or a comment on it added, edited or deleted. Any stale split proposal has been invalidated; its original tasks remain in your turn's split-invalid file and any existing children and creation receipts are retained by the host. Reassess the updated task before declaring a split again. As it stands now:\n\n{whole}"
                    )
                } else {
                    msg
                };
                if needs_owner && !human_reply {
                    continue;
                }
                let resumed = if msg.is_empty() {
                    Ok(())
                } else if human_reply {
                    reply_answer(self.cfg, &id, &msg, true)
                } else {
                    reply(self.cfg, &id, &msg, true, None)
                };
                if resumed.is_err() {
                    // Woken by what it awaits (proj _watch) since it was
                    // looked at, or its turn couldn't start: the reply waits
                    // for the next pass, in five minutes (a turn that did
                    // start kicks one sooner when it ends).
                    if filled(&job.path("session")) {
                        self.retry(repo);
                        continue;
                    }
                    // No session to continue (the first turn never got
                    // going): start over, from the issue as it stands,
                    // replies and all.
                    id = match self.start(repo, &now_issue, &comments) {
                        Ok(id) => id,
                        Err(f) => {
                            say(f);
                            self.retry(repo);
                            continue;
                        }
                    };
                    fs::write(d.join("job"), format!("{id}\n"))
                        .map_err(|e| io_fail(&d.join("job"), e))?;
                    fs::write(d.join("answered"), "0\n")
                        .map_err(|e| io_fail(&d.join("answered"), e))?;
                }
                if !msg.is_empty() {
                    Self::hand(&d, &whole)?;
                }
            }
            fs::write(d.join("seen"), format!("{newest}\n"))
                .map_err(|e| io_fail(&d.join("seen"), e))?;
        }
        Ok(())
    }
}

/// The turn the last carrying on of an issue's job started, and how many in a
/// row led to it: its record's revived, "TURN STREAK".
pub(super) fn revived(d: &Path) -> (Option<i64>, i64) {
    let rv = read(&d.join("revived")).unwrap_or_default();
    let (turn, streak) = rv.split_once(' ').unwrap_or((&rv, ""));
    (turn.parse().ok(), streak.parse().unwrap_or(0))
}
