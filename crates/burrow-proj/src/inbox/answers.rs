//! Completed answers survive immediate parking and retry delivery independently.
use super::{Pass, jobs::revived, trimmed};
use crate::{Fail, cmd::told, io_fail, job::Job, read};
use std::{fs, path::Path};

impl Pass<'_> {
    /// Publish completed turns in order. Retirement skips turns with no answer:
    /// stopping a running turn must not invent a completion for it.
    pub(super) fn answers(
        &mut self,
        repo: &str,
        n: &str,
        job: &Job,
        d: &Path,
        last: i64,
        state: Option<&str>,
    ) -> Result<bool, Fail> {
        let id = job.id;
        let turns = job.turns().and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
        let rv = revived(d);
        let mut k = read(&d.join("answered"))
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0);
        while k < last {
            k += 1;
            let why = if k == turns {
                format!(" ({})", state.unwrap_or("stopped"))
            } else {
                String::new()
            };
            let body = if rv.0 == Some(k + 1) {
                format!(
                    "The job's turn {k} was cut off before it answered (its whole unit was killed, out of memory as a rule): it carries on where it left off."
                )
            } else {
                match told(self.cfg, job.id, &k.to_string()) {
                    Ok(t) => trimmed(&t),
                    Err(_) if state.is_none() => {
                        fs::write(d.join("answered"), format!("{k}\n"))
                            .map_err(|e| io_fail(&d.join("answered"), e))?;
                        continue;
                    }
                    Err(_) => {
                        format!(
                            "The job's turn {k} ended without an answer{why}. `proj log {id} --turn {k} --raw` on {} has what it did.",
                            self.inbox.host
                        )
                    }
                }
            };
            // GitHub takes 65536 characters a comment.
            let engine = job.engine_label(&k.to_string());
            let body: String = format!("**{engine} job**\n\n{body}")
                .chars()
                .take(60000)
                .collect();
            if !self.post(repo, n, &body) {
                self.miss(repo);
                return Ok(false);
            }
            fs::write(d.join("answered"), format!("{k}\n"))
                .map_err(|e| io_fail(&d.join("answered"), e))?;
        }
        Ok(true)
    }
}
