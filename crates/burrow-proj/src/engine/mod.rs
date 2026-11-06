//! Engines own commands, sessions, streams and refusals; turns own lifecycle.

use crate::{config::Config, job::Job, read};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

pub(crate) mod claude;
mod claude_limit;
mod command;
mod role;
mod stream;
pub use command::{LONG, NEXT, exec_codex, spawn};
pub use role::role;
pub use stream::{Said, answer, last_result, names_session, stream_names_session};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    #[default]
    Claude,
    Codex,
}
impl Engine {
    pub fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Choice {
    pub engine: Engine,
    pub model: Option<String>,
}

/// Inbox jobs pin their label's engine; role jobs resolve the file each turn.
/// Judges always use Claude, so a role or overflow cannot make Codex judge itself.
pub fn choice(cfg: &Config, job: &Job) -> Result<Choice, String> {
    let role = read(&job.path("role")).unwrap_or_default();
    let mut c = cfg.engine(&role)?;
    if let Some(engine) = read(&job.path("engine")) {
        c = Choice::parse(&engine)?;
    }
    if role == "steward-judged" && c.engine == Engine::Codex {
        c = Choice::default();
    }
    Ok(c)
}

/// Normalized terminal records let the job lifecycle and log reader share outcomes.
pub fn result_of(line: &str) -> Option<Value> {
    if !line.contains(r#""type":"result""#) {
        return None;
    }
    let v: Value = serde_json::from_str(line).ok()?;
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

pub fn refused(
    engine: Engine,
    last: &Value,
    stream: &str,
    now: i64,
) -> Option<(Option<i64>, String)> {
    match engine {
        Engine::Claude => claude::refused(last, stream, now),
        Engine::Codex => {
            let said = last["result"].as_str().unwrap_or_default();
            (is_error(last) && burrow_codex::spent(said)).then(|| {
                (
                    Some(now.saturating_add(burrow_codex::wait_for(said) as i64)),
                    said.to_owned(),
                )
            })
        }
    }
}

/// A failed process is a failure even if it emitted an earlier success.
/// Codex can refuse login before JSON starts; only error output is examined.
/// A status of None means the turn stopped it (quiet, deadline or a lingering
/// answer): that stop belongs to the shared lifecycle, not an engine error.
pub fn ended(
    engine: Engine,
    stream: &Path,
    from: u64,
    status: Option<i32>,
    stderr: &str,
) -> Option<Value> {
    let last = last_result(stream, from);
    if engine != Engine::Codex
        || status.is_none_or(|s| s == 0)
        || last.as_ref().is_some_and(is_error)
    {
        return last;
    }
    let status = status.expect("a process that ended itself");
    let error = if stderr.trim().is_empty() {
        format!("codex exited {status}")
    } else {
        stderr.trim().to_owned()
    };
    Some(json!({"type":"result", "is_error":true, "result":error, "errors":[error]}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_judge_never_routes_to_codex_even_with_a_role_or_inbox_override() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
        cfg.jobs = dir.path().join("jobs");
        cfg.roles = dir.path().join("roles");
        fs::write(&cfg.roles, "steward-judged codex\n").unwrap();
        let job = Job::new(&cfg, "judge");
        fs::create_dir_all(&job.dir).unwrap();
        fs::write(job.path("role"), "steward-judged").unwrap();
        fs::write(job.path("engine"), "codex").unwrap();
        assert_eq!(choice(&cfg, &job).unwrap(), Choice::default());
        fs::remove_file(job.path("engine")).unwrap();
        fs::write(&cfg.roles, "steward-judged claude:chosen\n").unwrap();
        assert_eq!(choice(&cfg, &job).unwrap().model.as_deref(), Some("chosen"));
    }
}
