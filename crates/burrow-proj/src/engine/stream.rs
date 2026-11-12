//! Decode terminal answers without treating intermediate Codex messages as one.

use super::*;
use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader};

#[derive(Default)]
struct Decoder {
    message: String,
    failed: Option<String>,
}
impl Decoder {
    fn line(&mut self, line: &str) -> Option<Value> {
        if let Some(v) = result_of(line) {
            return Some(v);
        }
        let v: Value = serde_json::from_str(line).ok()?;
        match v["type"].as_str()? {
            "turn.started" => {
                self.message.clear();
                self.failed = None;
                None
            }
            "item.completed" if v["item"]["type"] == "agent_message" => {
                self.message = v["item"]["text"].as_str().unwrap_or_default().to_owned();
                None
            }
            "error" | "turn.failed" => {
                let error = v["message"]
                    .as_str()
                    .or(v["error"]["message"].as_str())
                    .unwrap_or("codex turn failed")
                    .to_owned();
                self.failed = Some(error.clone());
                Some(json!({"type":"result", "is_error":true, "result":error, "errors":[error]}))
            }
            "turn.completed" => Some(match &self.failed {
                Some(error) => {
                    json!({"type":"result", "is_error":true, "result":error, "errors":[error]})
                }
                None => json!({"type":"result", "is_error":false, "result":self.message}),
            }),
            _ => None,
        }
    }
}

/// Both formats name a session before work starts; keep the first nonempty id.
fn session(line: &str) -> Option<(Engine, String)> {
    let v: Value = serde_json::from_str(line).ok()?;
    [("session_id", Engine::Claude), ("thread_id", Engine::Codex)]
        .into_iter()
        .find_map(|(field, engine)| {
            v[field]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(|s| (engine, s.to_owned()))
        })
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
    let mut decoder = Decoder::default();
    let mut answer = String::new();
    if let Ok(lines) = lines(stream, 0) {
        for line in lines.map_while(Result::ok) {
            if let Some(v) = decoder.line(&line)
                && let Some(text) = v["result"].as_str().filter(|s| !s.is_empty())
            {
                if !answer.is_empty() {
                    answer.push_str("\n\n");
                }
                answer.push_str(text);
            }
        }
    }
    answer
}
pub fn last_result(stream: &Path, from: u64) -> Option<Value> {
    let mut decoder = Decoder::default();
    lines(stream, from)
        .ok()?
        .map_while(Result::ok)
        .filter_map(|line| decoder.line(&line))
        .last()
}

pub fn stream_tail(stream: &Path) -> String {
    let mut tail = VecDeque::new();
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
                    tail.push_back(line);
                }
            }
        }
    }
    tail.into_iter().collect()
}

/// Session storage is per engine; `session` remains the active session for job state.
/// A Claude session made before engine routing has no engine marker.
pub fn resume(job: &Job, engine: Engine) -> Option<String> {
    read(&job.path(&format!("session.{}", engine.name())))
        .or_else(|| {
            (engine == Engine::Claude && !job.path("session.engine").exists())
                .then(|| read(&job.path("session")))
                .flatten()
        })
        .filter(|s| !s.is_empty())
}

pub struct Said<'a> {
    job: &'a Job<'a>,
    file: Option<BufReader<File>>,
    partial: Vec<u8>,
    decoder: Decoder,
    pub answered: bool,
    pub named: bool,
}
impl<'a> Said<'a> {
    pub fn new(job: &'a Job<'a>, n: &str, from: u64) -> Self {
        let file = crate::state_reads::open(&job.turn(n, "jsonl"))
            .ok()
            .and_then(|mut f| {
                f.seek(SeekFrom::Start(from)).ok()?;
                Some(BufReader::new(f))
            });
        Self {
            job,
            file,
            partial: Vec::new(),
            decoder: Decoder::default(),
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
                if !self.job.path("session.engine").exists()
                    && let Some(previous) = read(&self.job.path("session"))
                {
                    let _ = crate::state_reads::write(
                        self.job.path("session.claude"),
                        format!("{previous}\n"),
                    );
                }
                let _ = crate::state_reads::write(
                    self.job.path(&format!("session.{}", engine.name())),
                    format!("{id}\n"),
                );
                let _ = crate::state_reads::write(self.job.path("session"), format!("{id}\n"));
                let _ = crate::state_reads::write(self.job.path("session.engine"), engine.name());
                self.named = true;
            }
            if self.decoder.line(&line).is_some() {
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
