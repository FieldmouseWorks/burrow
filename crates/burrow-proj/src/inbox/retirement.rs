//! Retirement is independent of notification delivery and of a reopened issue.
use std::{fs, path::Path};

use super::{Pass, dirs, say};
use crate::{Fail, cmd::halt, github::api, io_fail, read};

pub(super) fn records(state: &Path) -> Vec<std::path::PathBuf> {
    dirs(&state.join(".retired"), "")
        .into_iter()
        .flat_map(|repo| dirs(&repo, ""))
        .flat_map(|issue| dirs(&issue, ""))
        .collect()
}

impl Pass<'_> {
    pub(super) fn retired(&mut self, repo: &str) -> Result<(), Fail> {
        for d in records(&self.inbox.state) {
            let Some(issue) = read(&d.join("issue")) else {
                continue;
            };
            let Some((of, n)) = issue.rsplit_once('#') else {
                continue;
            };
            if of.eq_ignore_ascii_case(repo)
                && let Some(id) = read(&d.join("job"))
            {
                self.retire(repo, n, &id, &d)?;
            }
        }
        Ok(())
    }

    pub(super) fn retire(&mut self, repo: &str, n: &str, id: &str, d: &Path) -> Result<(), Fail> {
        // Always try to clear the old job's waits, even when GitHub is down.
        let stopped = if !self.cfg.jobs.join(id).is_dir() {
            true
        } else {
            let job = crate::job::Job::new(self.cfg, id);
            let _held = job.lock().map_err(|e| io_fail(&job.path("lock"), e))?;
            super::enrollment::remember(&job, repo, n).map_err(|e| Fail::new(1, e))?;
            match halt(self.cfg, &job, false) {
                Ok(()) => true,
                Err(f) => {
                    say(f);
                    false
                }
            }
        };
        if !stopped {
            self.retry(repo);
        }
        let job = crate::job::Job::new(self.cfg, id);
        let turns = job.turns().and_then(|n| n.parse::<i64>().ok()).unwrap_or(0);
        let answered = read(&d.join("answered"))
            .and_then(|n| n.parse::<i64>().ok())
            .unwrap_or(0);
        let mut snapshot = None;
        if stopped && answered < turns {
            let Some(issue) = api(self.cfg, &format!("repos/{repo}/issues/{n}")) else {
                self.miss(repo);
                return Ok(());
            };
            if self.eligible(repo, &issue) && !self.answers(repo, n, &job, d, turns, None)? {
                return Ok(());
            }
            snapshot = Some(issue);
        }
        if !d.join("reported").exists() {
            let Some(issue) =
                snapshot.or_else(|| api(self.cfg, &format!("repos/{repo}/issues/{n}")))
            else {
                self.miss(repo);
                return Ok(());
            };
            if self.eligible(repo, &issue) {
                let Some(written) = self.written(repo, &issue) else {
                    self.miss(repo);
                    return Ok(());
                };
                let seen = read(&d.join("seen"))
                    .and_then(|s| s.parse::<i64>().ok())
                    .unwrap_or(0);
                let mut unread: Vec<i64> = written
                    .comments
                    .iter()
                    .filter(|c| self.heard(c))
                    .filter_map(|c| c["id"].as_i64())
                    .filter(|id| *id > seen)
                    .collect();
                unread.sort_unstable();
                unread.dedup();
                // Include post()'s signature in the limit, leaving room below
                // GitHub's 65,536-character maximum. IDs, never bodies, are posted.
                let signature = self.signed("").chars().count();
                let header = "The job ended because this issue was closed or parked, with unread author replies:\n\n";
                let footer = "\nReopen the issue, remove `later` and restore an engine label to have them picked up.";
                let mut at = 0;
                while at < unread.len() {
                    let mut body = header.to_owned();
                    let mut end = at;
                    while end < unread.len() {
                        let link = format!(
                            "https://github.com/{repo}/issues/{n}#issuecomment-{}\n",
                            unread[end]
                        );
                        if body.chars().count()
                            + link.chars().count()
                            + footer.chars().count()
                            + signature
                            > 60000
                        {
                            break;
                        }
                        body.push_str(&link);
                        end += 1;
                    }
                    if end == at {
                        self.retry(repo);
                        return Ok(());
                    }
                    body.push_str(footer);
                    if !self.post(repo, n, &body) {
                        self.miss(repo);
                        return Ok(());
                    }
                    fs::write(d.join("seen"), format!("{}\n", unread[end - 1]))
                        .map_err(|e| io_fail(&d.join("seen"), e))?;
                    at = end;
                }
            }
            fs::write(d.join("reported"), "\n").map_err(|e| io_fail(&d.join("reported"), e))?;
        }
        if stopped {
            fs::remove_dir_all(d).map_err(|e| io_fail(d, e))?;
        }
        Ok(())
    }
}
