//! Decode terminal answers without treating intermediate Codex messages as one.

use super::*;

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
pub fn stream_names_session(stream: &Path, from: u64) -> bool {
    crate::state_reads::bytes(stream).is_ok_and(|b| {
        names_session(&String::from_utf8_lossy(
            b.get(from as usize..).unwrap_or_default(),
        ))
    })
}

/// Results in order, with only the last Codex message of each completed turn.
fn results(text: &str) -> Vec<Value> {
    let mut decoder = Decoder::default();
    text.lines().filter_map(|l| decoder.line(l)).collect()
}
pub fn answer(stream: &Path) -> String {
    let text = crate::state_reads::bytes(stream)
        .and_then(|b| String::from_utf8(b).map_err(std::io::Error::other))
        .unwrap_or_default();
    results(&text)
        .iter()
        .filter_map(|v| {
            v["result"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}
pub fn last_result(stream: &Path, from: u64) -> Option<Value> {
    let text = crate::state_reads::bytes(stream).ok()?;
    results(&String::from_utf8_lossy(
        text.get(from as usize..).unwrap_or_default(),
    ))
    .pop()
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
    file: Option<File>,
    partial: Vec<u8>,
    decoder: Decoder,
    pub answered: bool,
    pub named: bool,
}
impl<'a> Said<'a> {
    pub fn new(job: &'a Job<'a>, n: &str, from: u64) -> Self {
        let file = File::open(job.turn(n, "jsonl")).ok().and_then(|mut f| {
            f.seek(SeekFrom::Start(from)).ok()?;
            Some(f)
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
        let Some(f) = &mut self.file else {
            return;
        };
        if f.read_to_end(&mut self.partial).is_err() {
            return;
        }
        let end = if last {
            self.partial.len()
        } else {
            match self.partial.iter().rposition(|b| *b == b'\n') {
                Some(i) => i + 1,
                None => return,
            }
        };
        let whole: Vec<_> = self.partial.drain(..end).collect();
        for line in String::from_utf8_lossy(&whole).lines() {
            if !self.named
                && let Some((engine, id)) = session(line)
            {
                if !self.job.path("session.engine").exists()
                    && let Some(previous) = read(&self.job.path("session"))
                {
                    let _ = fs::write(self.job.path("session.claude"), format!("{previous}\n"));
                }
                let _ = fs::write(
                    self.job.path(&format!("session.{}", engine.name())),
                    format!("{id}\n"),
                );
                let _ = fs::write(self.job.path("session"), format!("{id}\n"));
                let _ = fs::write(self.job.path("session.engine"), engine.name());
                self.named = true;
            }
            if self.decoder.line(line).is_some() {
                self.answered = true;
            }
        }
    }
}

#[cfg(test)]
mod tests;
