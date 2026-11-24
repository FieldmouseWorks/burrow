//! Engine-agnostic job transcript (`N.jsonl`) helpers: session persistence,
//! resume lookup, normalized result/refusal readers, incremental watching,
//! and redacted tail views.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;

use serde_json::Value;

use super::{DEFAULT_ENGINE, conversation_of};
use crate::host_jobs::View;
use crate::job::Job;
use crate::read;
use crate::record::{JobFile, TurnFile};

/// Normalized terminal records let the job lifecycle and log reader share
/// outcomes across every engine plugin (and pre-deploy Claude streams).
pub fn result_of(line: &str) -> Option<Value> {
    if !line.contains(r#""type":"result""#) && !line.contains(r#""type": "result""#) {
        return None;
    }
    let v: Value = serde_json::from_str(line).ok()?;
    if v.get("type").and_then(Value::as_str) != Some("result") {
        return None;
    }
    let said = match v.get("result") {
        None | Some(Value::Null) | Some(Value::Bool(false)) => false,
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    };
    let idle = v.get("num_turns").and_then(Value::as_f64) == Some(0.0);
    (said || !idle || is_error(&v)).then_some(v)
}

pub fn is_error(result: &Value) -> bool {
    !matches!(
        result.get("is_error"),
        None | Some(Value::Null | Value::Bool(false))
    )
}

/// First non-empty session identifier carried by a normalized transcript line.
pub fn session_id(line: &str) -> Option<String> {
    let v: Value = serde_json::from_str(line).ok()?;
    v["session_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// One pinned, confined stream, sought before parsing a retry's records.
pub fn lines(stream: &Path, from: u64) -> io::Result<io::Lines<BufReader<File>>> {
    let mut file = crate::state_reads::open(stream)?;
    file.seek(SeekFrom::Start(from))?;
    Ok(BufReader::new(file).lines())
}

/// The session a turn's stream names, as its engine named it first.
pub fn stream_session(stream: &Path) -> Option<String> {
    lines(stream, 0)
        .ok()?
        .map_while(Result::ok)
        .find_map(|l| session_id(&l))
}

pub fn names_session(text: &str) -> bool {
    text.lines().any(|l| session_id(l).is_some())
}

pub fn stream_names_session(stream: &Path, from: u64) -> bool {
    lines(stream, from).is_ok_and(|lines| {
        lines
            .map_while(Result::ok)
            .any(|l| session_id(&l).is_some())
    })
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

/// Read a normalized `burrow.refusal` record (or legacy pre-deploy rate-limit
/// refusal record) from `path` starting at `from`, used by deploy-overlap
/// retained retry checkpoint recovery (`retry.rs`).
pub fn refused_file(path: &Path, from: u64) -> Option<(Option<i64>, String)> {
    let mut normalized: Option<(Option<i64>, String)> = None;
    let mut legacy_until: Option<i64> = None;
    let mut legacy_reason: Option<String> = None;
    let mut saw_legacy_limit = false;

    for line in lines(path, from).ok()?.map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if v.get("type").and_then(Value::as_str) == Some("burrow.refusal") {
            let kind = v.get("kind").and_then(Value::as_str).unwrap_or_default();
            if matches!(kind, "usage_limit" | "login_expired") {
                let until = v.get("resets_at").and_then(Value::as_i64);
                let reason = v
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("engine usage limit reached")
                    .to_owned();
                normalized = Some((until, reason));
            }
            continue;
        }
        for info in [
            &v["api_error_params"]["rate_limit_info"],
            &v["rate_limit_info"],
        ] {
            if info["status"] == "rejected" {
                saw_legacy_limit = true;
                legacy_until = legacy_until.max(info["resetsAt"].as_i64());
            }
        }
        if v.get("type").and_then(Value::as_str) == Some("result") && is_error(&v) {
            let said = v["result"].as_str().unwrap_or_default().trim();
            let lower = said.to_lowercase();
            if v["api_error"] == "usage_limit_reached"
                || saw_legacy_limit
                || lower.contains("limit reached")
                || lower.contains("hit your")
                || lower.contains("spend limit")
                || lower.contains("usage limit")
                || lower.contains("out of extra usage")
            {
                saw_legacy_limit = true;
                legacy_reason = Some(if said.is_empty() {
                    "usage limit reached".to_owned()
                } else {
                    said.to_owned()
                });
            }
        }
    }

    normalized
        .or_else(|| saw_legacy_limit.then(|| (legacy_until, legacy_reason.unwrap_or_default())))
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

/// Session storage is per conversation (`claude` and `claude-api` share `claude`;
/// any other engine keeps its own `session.<engine>`).
pub fn resume_for(job: &Job, engine: &str) -> Option<String> {
    let conv = conversation_of(engine);
    read(&job.session_file(conv))
        .or_else(|| {
            (conv == DEFAULT_ENGINE && !job.file(JobFile::SessionEngine).exists())
                .then(|| read(&job.file(JobFile::Session)))
                .flatten()
        })
        .filter(|s| !s.is_empty())
}

/// Persist `session_id` for `engine`'s conversation, preserving any unlabelled
/// legacy `session` under `session.claude` before overwriting `session`.
pub fn record_session(job: &Job, engine: &str, id: &str) {
    if id.trim().is_empty() {
        return;
    }
    let conv = conversation_of(engine);
    if !job.file(JobFile::SessionEngine).exists()
        && let Some(previous) = read(&job.file(JobFile::Session))
        && !previous.is_empty()
    {
        let _ =
            crate::state_reads::write(job.session_file(DEFAULT_ENGINE), format!("{previous}\n"));
    }
    let _ = crate::state_reads::write(job.session_file(conv), format!("{id}\n"));
    let _ = crate::state_reads::write(job.file(JobFile::Session), format!("{id}\n"));
    let _ = crate::state_reads::write(job.file(JobFile::SessionEngine), conv);
}

/// Incremental reader over a live turn JSONL file, tracking the first named
/// session id and whether a terminal result record has appeared.
struct Watcher {
    file: Option<BufReader<File>>,
    partial: Vec<u8>,
    session: Option<String>,
    answered: bool,
}

impl Watcher {
    fn new(stream: &Path, from: u64) -> Self {
        let file = crate::state_reads::open(stream).ok().and_then(|mut f| {
            f.seek(SeekFrom::Start(from)).ok()?;
            Some(BufReader::new(f))
        });
        Self {
            file,
            partial: Vec::new(),
            session: None,
            answered: false,
        }
    }

    fn look(&mut self, last: bool) {
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
            if self.session.is_none()
                && let Some(id) = session_id(&line)
            {
                self.session = Some(id);
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

pub struct Said<'a> {
    job: &'a Job<'a>,
    engine: String,
    watcher: Watcher,
    pub answered: bool,
    pub named: bool,
}

impl<'a> Said<'a> {
    pub fn new(job: &'a Job<'a>, n: &str, from: u64, engine: &str) -> Self {
        Self {
            job,
            engine: engine.to_owned(),
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
            record_session(self.job, &self.engine, id);
            self.named = true;
        }
        self.answered = self.watcher.answered;
    }
}

#[cfg(test)]
mod tests;
