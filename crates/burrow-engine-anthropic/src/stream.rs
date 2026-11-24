//! Claude Code stream JSONL parsing: sessions, terminal results, errors,
//! and incremental reading.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;

use burrow_process::state_reads;
use serde_json::Value;

/// Normalized terminal records let the job lifecycle and log reader share outcomes.
pub fn result_of(line: &str) -> Option<Value> {
    // A cheap look before parsing, whatever the record's spacing: its type's
    // value is the string "result" wherever it sits.
    if !line.contains(r#""result""#) {
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

/// The engine names its session before work starts; keep the first nonempty id.
pub fn session_id(line: &str) -> Option<String> {
    let v: Value = serde_json::from_str(line).ok()?;
    v["session_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// One pinned, confined stream, sought before parsing a retry's records.
pub fn lines(stream: &Path, from: u64) -> io::Result<io::Lines<BufReader<File>>> {
    let mut file = state_reads::open(stream)?;
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

/// Incremental reader over a live Claude stream JSONL file, tracking the first
/// named session id and whether a terminal result record has appeared.
pub struct Watcher {
    file: Option<BufReader<File>>,
    partial: Vec<u8>,
    pub session: Option<String>,
    pub answered: bool,
}

impl Watcher {
    pub fn new(stream: &Path, from: u64) -> Self {
        let file = state_reads::open(stream).ok().and_then(|mut f| {
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

#[cfg(test)]
mod tests;
