//! Sorting questions stay with their job until a person answers or it resolves.
use super::{Pass, raw, seconds};
use crate::inbox::record::files::{self, File};
use crate::record::{JobFile, TurnFile};
use crate::{Fail, ask, cmd::reply_answer, job::Job, read};
use serde_json::Value;
use std::path::Path;

impl Pass<'_> {
    /// An outstanding question is never a sorting verdict. Timed-out one-way
    /// questions retain a reply route, but no longer occupy a sorting slot.
    #[expect(clippy::too_many_lines, reason = "legacy; split by job")]
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
                .map_err(|e| crate::io_fail(&job.file(JobFile::Lock), e))?;
            if let Err(e) = crate::question::recover(job) {
                eprintln!("proj sorting question recovery: {e}");
                self.retry(repo);
                return Ok(true);
            }
        }
        let pending = ask::host_path(job, "question-needs-owner").exists();
        // Unreadable authority holds the question: retry, never fall through
        // to the verdict and retire it.
        let asked: Option<ask::Question> = match ask::load(job) {
            Ok(question) => question,
            Err(e) => {
                eprintln!("proj sorting question: {e}");
                self.retry(repo);
                return Ok(true);
            }
        };
        if asked.is_none() && !pending {
            return Ok(false);
        }
        if let Err(e) = ask::visibility(self.cfg, job) {
            eprintln!("proj sorting question: {e}");
            self.retry(repo);
        }
        let identity = read(&ask::host_path(job, "question-id"))
            .ok_or_else(|| Fail::new(1, "sorting question has no host identity"))?;
        let notice = format!("{identity}\t{}", if pending { "one-way" } else { "ask" });
        if files::question_notice(d).as_deref() != Some(notice.as_str()) {
            let question = read(&ask::host_path(job, "question-text")).unwrap_or_default();
            let body = if pending {
                format!(
                    "{question}\n\n{}",
                    job.turns()
                        .and_then(|t| read(&job.turn_file(&t, TurnFile::Result)))
                        .unwrap_or_default()
                )
            } else {
                format!(
                    "{question}{}",
                    asked
                        .as_ref()
                        .map(ask::Question::notice)
                        .unwrap_or_default()
                )
            };
            if self.post(repo, n, &body) {
                files::mark(d, File::QuestionNotice)
                    .write(&notice)
                    .map_err(|e| crate::io_fail(d, e))?;
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
        let seen = files::seen(d)
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
        files::mark(d, File::Seen)
            .write(newest.to_string())
            .map_err(|e| crate::io_fail(d, e))?;
        Ok(true)
    }
}
