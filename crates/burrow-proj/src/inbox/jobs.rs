//! A pass's look at the jobs it already has for a repo's issues: each turn's
//! answer posted, a cut-off turn carried on, the job ended with its issue,
//! and replies handed on.

use std::fs;
use std::path::Path;

use serde_json::Value;

use super::{CUT_OFF, Pass, dirs, key, raw, say, seconds, trimmed};
use crate::cmd::{reply, stop, told, unstick};
use crate::github::{api, every};
use crate::job::Job;
use crate::{Fail, filled, io_fail, now, read};

impl Pass<'_> {
    /// Jobs already running for this repo's issues.
    pub(super) fn jobs(&mut self, repo: &str, p: &str, open: &[Value]) -> Result<(), Fail> {
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
            if st == "failed" && k < turns && streak < 2 && now_open.is_some() {
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
                let body: String = body.chars().take(60000).collect();
                if !self.post(repo, n, &body) {
                    self.miss(repo);
                    continue 'jobs;
                }
                fs::write(d.join("answered"), format!("{k}\n"))
                    .map_err(|e| io_fail(&d.join("answered"), e))?;
            }
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

            // Closed, or the label taken off: done with it, and so is its
            // job, which nothing it awaits brings back. A job that couldn't
            // be stopped (proj stop says why) is tried again in five minutes;
            // the rest are still seen to.
            if !open.iter().any(|i| raw(&i["number"]) == n) {
                if let Err(f) = stop(self.cfg, &id, false) {
                    say(f);
                    self.retry(repo);
                    continue;
                }
                let _ = fs::remove_dir_all(&d);
                continue;
            }

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
            let Some(all) = every(self.cfg, &format!("repos/{repo}/issues/{n}/comments")) else {
                self.miss(repo);
                continue;
            };
            let before = jiff::Timestamp::from_second(now() - 60)
                .map(|t| t.strftime("%Y-%m-%dT%H:%M:%SZ").to_string())
                .unwrap_or_default();
            let made = |c: &Value| c["created_at"].as_str().unwrap_or_default().to_owned();
            let after = |c: &Value| c["id"].as_i64().is_some_and(|i| i > seen);
            let comments: Vec<Value> = all.iter().filter(|c| made(c) < before).cloned().collect();
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
            if !msg.is_empty() {
                let Some(now_issue) = api(self.cfg, &[&format!("repos/{repo}/issues/{n}")]) else {
                    self.miss(repo);
                    continue;
                };
                let labelled = now_issue["labels"].as_array().is_some_and(|ls| {
                    ls.iter()
                        .any(|l| l["name"].as_str() == Some(self.inbox.label.as_str()))
                });
                if now_issue["state"] != "open" || !labelled {
                    continue;
                }
                if reply(self.cfg, &id, &msg, true, None).is_err() {
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
