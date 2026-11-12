//! Issue labels govern both enrollment and every later turn.
use super::{Pass, dirs, key, raw, say, written};
use crate::{Fail, config::Inbox, github::api, io_fail, job::Job, read};
use serde_json::Value;
use std::{fs, path::Path};

/// Missing labels are an unreadable response, never evidence of removal.
pub(super) fn parked(inbox: &Inbox, issue: &Value) -> Result<Option<&'static str>, String> {
    let labels = issue["labels"]
        .as_array()
        .ok_or("issue labels are unavailable")?;
    let names = labels
        .iter()
        .map(|l| l["name"].as_str().ok_or("issue label name is unavailable"))
        .collect::<Result<Vec<_>, _>>()?;
    if names.contains(&super::triage::LATER) {
        Ok(Some("its issue is labelled later"))
    } else if !names.iter().any(|name| inbox.labels.contains_key(*name)) {
        Ok(Some("its issue has no engine label"))
    } else {
        Ok(None)
    }
}

/// Only inbox workers are governed by engine labels. Sorting workers and
/// standalone jobs have their own admission. The record covers old workers
/// that predate the inbox-issue marker; modern ones keep their source after
/// retirement, so even an explicit reply cannot revive the stopped worker.
fn source(job: &Job) -> Result<Option<(String, String)>, String> {
    if let Some(source) = read(&crate::ask::host_path(job, "inbox-issue")) {
        return crate::github::target(&source)
            .map(Some)
            .ok_or_else(|| format!("proj: {}'s inbox issue is invalid", job.id));
    }
    if job.path("inbox-issue").exists() {
        let source =
            read(&crate::ask::host_path(job, "source")).or_else(|| read(&job.path("link")));
        return source
            .as_deref()
            .and_then(crate::github::target)
            .map(Some)
            .ok_or_else(|| format!("proj: {}'s inbox issue is unavailable", job.id));
    }
    let Some(inbox) = &job.cfg.inbox else {
        return Ok(None);
    };
    let source = dirs(&inbox.state, "")
        .iter()
        .find(|d| read(&d.join("job")).as_deref() == Some(job.id))
        .and_then(|d| read(&d.join("issue")))
        .as_deref()
        .and_then(crate::github::target);
    if let Some((repo, n)) = &source {
        remember(job, repo, n)?;
    }
    Ok(source)
}

/// Preserve legacy association after the inbox record is retired.
fn remember(job: &Job, repo: &str, n: &str) -> Result<(), String> {
    crate::ask::ensure(job)?;
    crate::swap(
        &crate::ask::host_path(job, "inbox-issue"),
        &format!("{repo}#{n}\n"),
    )
    .map_err(|e| e.to_string())
}

/// Under the turn-start lock, before writing a prompt or consuming a question.
/// Stopping here closes the race with an inbox label event not yet processed.
pub(crate) fn admit(job: &Job) -> Result<(), String> {
    // The inbox admits the initial verified snapshot. Only resumptions need
    // another label read; never replace the worker's approved text with it.
    if job.turns().is_none() {
        return Ok(());
    }
    let Some((repo, n)) = source(job)? else {
        return Ok(());
    };
    if job
        .turns()
        .is_some_and(|n| job.turn(&n, "stopped").exists())
    {
        return Err(format!(
            "proj: {} is stopped; relabelling starts a fresh job",
            job.id
        ));
    }
    let inbox = job
        .cfg
        .inbox
        .as_ref()
        .ok_or("proj: inbox configuration is unavailable")?;
    let issue = api(job.cfg, &format!("repos/{repo}/issues/{n}"))
        .ok_or_else(|| format!("proj: cannot check {repo}#{n}'s labels; no turn started"))?;
    if let Some(why) = parked(inbox, &issue)? {
        let message = format!(
            "proj: stopping {}: {why} ({repo}#{n}); no turn started",
            job.id
        );
        eprintln!("{message}");
        crate::ask::ensure(job)?;
        crate::swap(&crate::ask::host_path(job, "inbox-parked"), why).map_err(|e| e.to_string())?;
        crate::cmd::halt(job.cfg, job, false).map_err(|e| {
            e.say
                .unwrap_or_else(|| format!("proj stop failed (exit {})", e.code))
        })?;
        return Err(message);
    }
    Ok(())
}

impl Pass<'_> {
    /// Reconcile parking before posting answers, recovering questions or
    /// skipping a running/repository-paused job. A missing list entry is
    /// checked directly: losing the last engine label removes it from the list.
    pub(super) fn stop_parked(
        &mut self,
        repo: &str,
        n: &str,
        id: &str,
        d: &Path,
        listed: Option<&Value>,
    ) -> Result<bool, Fail> {
        let fetched;
        let issue = match listed {
            Some(issue) => issue,
            None => {
                fetched = api(self.cfg, &format!("repos/{repo}/issues/{n}"));
                let Some(issue) = fetched.as_ref() else {
                    self.miss(repo);
                    return Ok(true);
                };
                issue
            }
        };
        match parked(self.inbox, issue) {
            Ok(None) => Ok(false),
            Ok(Some(why)) => {
                eprintln!("proj-inbox: stopping {id}: {why} ({repo}#{n})");
                {
                    let job = Job::new(self.cfg, id);
                    let _held = job.lock().map_err(|e| io_fail(&job.path("lock"), e))?;
                    remember(&job, repo, n).map_err(|e| Fail::new(1, e))?;
                }
                self.retire_record(repo, n, id, d)?;
                Ok(true)
            }
            Err(why) => {
                eprintln!("proj-inbox: cannot check {repo}#{n}: {why}");
                self.miss(repo);
                Ok(true)
            }
        }
    }

    pub(super) fn retire_record(
        &mut self,
        repo: &str,
        n: &str,
        id: &str,
        d: &Path,
    ) -> Result<(), Fail> {
        let root = self
            .inbox
            .state
            .join(".retired")
            .join(key(repo).to_lowercase())
            .join(n);
        fs::create_dir_all(&root).map_err(|e| io_fail(&root, e))?;
        let retired = root.join(id);
        fs::rename(d, &retired).map_err(|e| io_fail(&retired, e))?;
        self.retire(repo, n, id, &retired)
    }

    /// New issues become jobs.
    pub(super) fn new_issues(&mut self, repo: &str, open: &[Value]) -> Result<(), Fail> {
        for issue in open {
            let n = raw(&issue["number"]);
            let d = self.inbox.state.join(format!("{}-{n}", key(repo)));
            if !matches!(parked(self.inbox, issue), Ok(None)) {
                continue;
            }
            let retiring = self
                .inbox
                .state
                .join(".retired")
                .join(key(repo).to_lowercase())
                .join(&n);
            if dirs(&retiring, "").iter().any(|d| {
                read(&d.join("job")).is_some_and(|id| {
                    let job = Job::new(self.cfg, &id);
                    job.dir.is_dir()
                        && !job
                            .turns()
                            .is_some_and(|n| job.turn(&n, "stopped").exists())
                })
            }) {
                self.retry(repo);
                continue;
            }
            if let Some(id) = read(&d.join("job")) {
                let job = Job::new(self.cfg, &id);
                if !crate::ask::host_path(&job, "inbox-parked").exists()
                    || !job
                        .turns()
                        .is_some_and(|n| job.turn(&n, "stopped").exists())
                {
                    continue;
                }
                self.retire_record(repo, &n, &id, &d)?;
            }
            let opener = raw(&issue["user"]["login"]);
            if !self.opened_by_ours(issue) && !self.generated_child(repo, issue) {
                let said = format!(
                    "No job for this issue: it was opened by `{opener}`, and only an issue opened by one of {}'s authors ({}) starts one. For a job to start, one of them has to open the issue.",
                    self.inbox.host,
                    burrow_github::author_names(&self.inbox.authors, ", "),
                );
                self.refuse(repo, &n, &said, &d)?;
                continue;
            }
            if self.outside(issue) {
                let said = format!(
                    "No job for this issue: it's labelled `{}`, so it carries text from outside {}'s authors, whoever opened it, and no agent reads it. To hand the work on, open an issue of your own that says what to do.",
                    self.inbox.outside_label, self.inbox.host,
                );
                self.refuse(repo, &n, &said, &d)?;
                continue;
            }
            // Comments already there are part of the task; replies after
            // this are turns.
            let Some(written::Written {
                issue: safe,
                comments,
                ..
            }) = self.written(repo, issue)
            else {
                self.miss(repo);
                continue;
            };
            let issue = &safe;
            if self.inbox.planning
                && (!self.approved_input(repo, &n, &self.issue_text(repo, issue, &comments))
                    || !self.child_admissible(repo, &n))
            {
                continue;
            }
            let id = match self.start(repo, issue, &comments) {
                Ok(id) => id,
                Err(f) => {
                    say(f);
                    self.retry(repo);
                    continue;
                }
            };
            let seen = comments.iter().filter_map(|c| c["id"].as_i64()).max();
            let write = |name: &str, text: String| {
                fs::write(d.join(name), text).map_err(|e| io_fail(&d.join(name), e))
            };
            fs::create_dir_all(&d).map_err(|e| io_fail(&d, e))?;
            write("issue", format!("{repo}#{n}\n"))?;
            write("job", format!("{id}\n"))?;
            write("answered", "0\n".into())?;
            write("seen", format!("{}\n", seen.unwrap_or(0)))?;
            // What it was handed, so it's closed with the issue only as long
            // as the issue is still that (jobs.rs).
            Self::hand(&d, &self.issue_text(repo, issue, &comments))?;
            // Said on the issue, or tried again next pass.
            write("unannounced", String::new())?;
        }
        Ok(())
    }
}
