//! Job-side stream helpers: session persistence, resume lookup, and redacted
//! tail views. Claude JSONL parsing itself lives in `burrow_engine_anthropic::stream`.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::path::Path;

use burrow_engine_anthropic::stream::Watcher;

use super::*;
use crate::host_jobs::View;
use crate::record::{JobFile, TurnFile};

/// The stream's last three records. The bridge's view redacts them, reading
/// earlier records too: the visible tail may start in the middle of an
/// earlier secret payload.
pub fn stream_tail(stream: &Path, view: View) -> String {
    let mut tail = VecDeque::new();
    let mut redactor = burrow_probe::scrub::Jsonl::default();
    if let Ok(file) = crate::state_reads::open(stream) {
        let mut reader = BufReader::new(file);
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if tail.len() == 3 {
                        tail.pop_front();
                    }
                    tail.push_back(match view {
                        View::Owner => line,
                        View::Bridge => redactor
                            .line(&line)
                            .unwrap_or_else(|_| "[redacted]\n".into()),
                    });
                }
            }
        }
    }
    tail.into_iter().collect()
}

/// Session storage is per conversation (`claude` and `claude-api` share `claude`;
/// any other engine keeps its own `session.<engine>`).
pub fn resume_for(job: &Job, engine: &str) -> Option<String> {
    let conv = super::conversation_of(engine);
    read(&job.dir.join(format!("session.{conv}")))
        .or_else(|| {
            (conv == "claude" && !job.file(JobFile::SessionEngine).exists())
                .then(|| read(&job.file(JobFile::Session)))
                .flatten()
        })
        .filter(|s| !s.is_empty())
}

/// Session storage is per conversation ([`Engine::conversation`]); `session` remains the active session for job state.
/// A Claude session made before engine routing has no engine marker.
pub fn resume(job: &Job, engine: Engine) -> Option<String> {
    resume_for(job, engine.name())
}

pub struct Said<'a> {
    job: &'a Job<'a>,
    watcher: Watcher,
    pub answered: bool,
    pub named: bool,
}

impl<'a> Said<'a> {
    pub fn new(job: &'a Job<'a>, n: &str, from: u64) -> Self {
        Self {
            job,
            watcher: Watcher::new(&job.turn_file(n, TurnFile::Jsonl), from),
            answered: false,
            named: false,
        }
    }

    pub fn look(&mut self, last: bool) {
        self.watcher.look(last);
        if !self.named
            && let Some(id) = self.watcher.session.as_deref()
        {
            if !self.job.file(JobFile::SessionEngine).exists()
                && let Some(previous) = read(&self.job.file(JobFile::Session))
            {
                let _ = crate::state_reads::write(
                    self.job.file(JobFile::SessionOf(Engine::Claude)),
                    format!("{previous}\n"),
                );
            }
            let _ = crate::state_reads::write(
                self.job.file(JobFile::SessionOf(Engine::Claude)),
                format!("{id}\n"),
            );
            let _ = crate::state_reads::write(self.job.file(JobFile::Session), format!("{id}\n"));
            let _ = crate::state_reads::write(
                self.job.file(JobFile::SessionEngine),
                Engine::Claude.name(),
            );
            self.named = true;
        }
        self.answered = self.watcher.answered;
    }
}

#[cfg(test)]
mod tests;
