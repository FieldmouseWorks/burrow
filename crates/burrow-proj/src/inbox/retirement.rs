//! Retirement is independent of notification delivery and of a reopened issue.
use std::{fs, path::Path};

use super::{Pass, dirs, key, say};
use crate::{Fail, cmd::stop, github::api, io_fail, read};

impl Pass<'_> {
    pub(super) fn retired(&mut self, repo: &str) -> Result<(), Fail> {
        for d in dirs(
            &self.inbox.state.join(".retired"),
            &format!("{}-", key(repo)),
        ) {
            let Some(issue) = read(&d.join("issue")) else {
                continue;
            };
            let Some((of, n)) = issue.rsplit_once('#') else {
                continue;
            };
            if of == repo
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
            match stop(self.cfg, id, false) {
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
        if !d.join("reported").exists() {
            let Some(issue) = api(self.cfg, &[&format!("repos/{repo}/issues/{n}")]) else {
                self.miss(repo);
                return Ok(());
            };
            if self.eligible(&issue) {
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
                let header = "The job ended because this issue was closed or its job label was removed, with unread author replies:\n\n";
                let footer = "\nReopen the issue and restore its job label to have them picked up.";
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
