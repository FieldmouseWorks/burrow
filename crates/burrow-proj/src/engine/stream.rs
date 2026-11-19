//! A turn's stream: the session it names and the answers it holds.

use super::*;
use crate::host_jobs::View;
use crate::record::{JobFile, TurnFile};
use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader};

/// The engine names its session before work starts; keep the first nonempty id.
fn session(line: &str) -> Option<(Engine, String)> {
    let v: Value = serde_json::from_str(line).ok()?;
    v["session_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(|s| (Engine::Claude, s.to_owned()))
}
/// The session a turn's stream names, as its engine named it first.
pub fn stream_session(stream: &Path) -> Option<String> {
    lines(stream, 0)
        .ok()?
        .map_while(Result::ok)
        .find_map(|l| session(&l))
        .map(|(_, id)| id)
}
pub fn names_session(text: &str) -> bool {
    text.lines().any(|l| session(l).is_some())
}
/// One pinned, confined stream, sought before parsing a retry's records.
pub(super) fn lines(stream: &Path, from: u64) -> io::Result<io::Lines<BufReader<File>>> {
    let mut file = crate::state_reads::open(stream)?;
    file.seek(SeekFrom::Start(from))?;
    Ok(BufReader::new(file).lines())
}
pub fn stream_names_session(stream: &Path, from: u64) -> bool {
    lines(stream, from)
        .is_ok_and(|lines| lines.map_while(Result::ok).any(|l| session(&l).is_some()))
}

pub fn answer(stream: &Path) -> String {
    results(stream).join("\n\n")
}
/// Each non-empty result a turn's stream holds, in order: the pieces its
/// answer is made of, kept apart for a view that redacts each on its own.
pub fn results(stream: &Path) -> Vec<String> {
    let mut results = Vec::new();
    if let Ok(lines) = lines(stream, 0) {
        for line in lines.map_while(Result::ok) {
            if let Some(v) = result_of(&line)
                && let Some(text) = v["result"].as_str().filter(|s| !s.is_empty())
            {
                results.push(text.to_owned());
            }
        }
    }
    results
}
pub fn last_result(stream: &Path, from: u64) -> Option<Value> {
    lines(stream, from)
        .ok()?
        .map_while(Result::ok)
        .filter_map(|line| result_of(&line))
        .last()
}

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

/// Session storage is per engine; `session` remains the active session for job state.
/// A Claude session made before engine routing has no engine marker.
pub fn resume(job: &Job, engine: Engine) -> Option<String> {
    read(&job.file(JobFile::SessionOf(engine)))
        .or_else(|| {
            (engine == Engine::Claude && !job.file(JobFile::SessionEngine).exists())
                .then(|| read(&job.file(JobFile::Session)))
                .flatten()
        })
        .filter(|s| !s.is_empty())
}

pub struct Said<'a> {
    job: &'a Job<'a>,
    file: Option<BufReader<File>>,
    partial: Vec<u8>,
    pub answered: bool,
    pub named: bool,
}
impl<'a> Said<'a> {
    pub fn new(job: &'a Job<'a>, n: &str, from: u64) -> Self {
        let file = crate::state_reads::open(&job.turn_file(n, TurnFile::Jsonl))
            .ok()
            .and_then(|mut f| {
                f.seek(SeekFrom::Start(from)).ok()?;
                Some(BufReader::new(f))
            });
        Self {
            job,
            file,
            partial: Vec::new(),
            answered: false,
            named: false,
        }
    }
    pub fn look(&mut self, last: bool) {
        loop {
            let Some(f) = &mut self.file else {
                return;
            };
            let Ok(count) = f.read_until(b'\n', &mut self.partial) else {
                return;
            };
            if self.partial.is_empty() || (!last && !self.partial.ends_with(b"\n")) {
                return;
            }
            let whole = std::mem::take(&mut self.partial);
            let line = String::from_utf8_lossy(&whole);
            if !self.named
                && let Some((engine, id)) = session(&line)
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
                    self.job.file(JobFile::SessionOf(engine)),
                    format!("{id}\n"),
                );
                let _ =
                    crate::state_reads::write(self.job.file(JobFile::Session), format!("{id}\n"));
                let _ =
                    crate::state_reads::write(self.job.file(JobFile::SessionEngine), engine.name());
                self.named = true;
            }
            if result_of(&line).is_some() {
                self.answered = true;
            }
            self.partial = whole;
            self.partial.clear();
            if count == 0 {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests;
