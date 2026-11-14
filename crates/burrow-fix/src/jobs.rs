//! The proj job a fix runs as (Conary's `docs/m6-plugins.md`, F): started
//! through the job runner, which keeps the slots, engines, sandboxes, the
//! acting App and usage overflow; read through its record whole
//! (`proj why JOB --json`); stopped with `proj stop`. Never `proj
//! lifecycle`, `proj state`'s snapshot or an `--owned` job: those go with
//! the steward.
use crate::Config;
use serde::Deserialize;
use std::process::{Command, Stdio};

/// What a fix needs of the job runner.
pub trait Jobs: Send + Sync {
    /// A new job on `repo`'s clone for `task`, held under `key` (proj
    /// refuses a key a live job holds): its id.
    fn start(&self, repo: &str, key: &str, task: &str) -> Result<String, String>;
    /// The job holding `key`, if any.
    fn find(&self, key: &str) -> Result<Option<String>, String>;
    /// The job's record, as `proj why --json` says it.
    fn why(&self, id: &str) -> Result<Record, String>;
    /// Stop the job, queued or running, and anything it awaits.
    fn stop(&self, id: &str) -> Result<(), String>;
    /// What the job left in its answers file, if it left one.
    fn answers(&self, id: &str) -> Result<Option<Vec<u8>>, String>;
}

/// A job's record, the parts a fix reads of `proj why --json`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Record {
    /// parked, queued, running, or how its latest turn ended.
    pub stage: String,
    /// Past this, the job is a stall: the fix's own deadline.
    pub overdue_at: Option<i64>,
    /// How its latest turn said it ends, a line each.
    #[serde(default)]
    pub ends: Vec<String>,
    /// Its latest turn, once it has one.
    pub turn: Option<Turn>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Turn {
    pub engine: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub session: Option<String>,
}

impl Record {
    /// Whether its turn is still to end: parked, queued or running.
    pub fn live(&self) -> bool {
        matches!(self.stage.as_str(), "parked" | "queued" | "running")
    }

    /// Whether its turn ended done, and holds nothing open: no await, no
    /// question. A fix is one turn.
    pub fn done(&self) -> bool {
        self.stage == "done" && self.ends.iter().all(|l| l == "done")
    }
}

/// proj, as the machine runs it.
pub struct Proj(pub Config);

impl Proj {
    fn run(&self, args: &[&str]) -> Result<String, String> {
        let (program, before) = self.0.proj.split_first().ok_or("no proj is configured")?;
        let out = Command::new(program)
            .args(before)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("proj doesn't start: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "proj {} failed: {}",
                args.first().unwrap_or(&""),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    }
}

impl Jobs for Proj {
    fn start(&self, repo: &str, key: &str, task: &str) -> Result<String, String> {
        let c = &self.0;
        let mut args = vec!["run", "--pipeline", "--role", &c.role, "--key", key];
        if let Some(app) = &c.app {
            args.extend(["--app", app]);
        }
        args.extend(["--timeout", &c.timeout, "--idle", &c.idle, repo, task]);
        let id = self.run(&args)?;
        if id.is_empty() || id.contains(char::is_whitespace) {
            return Err(format!("proj run named no job: {id:?}"));
        }
        Ok(id)
    }

    fn find(&self, key: &str) -> Result<Option<String>, String> {
        let id = self.run(&["find", key])?;
        Ok((!id.is_empty()).then_some(id))
    }

    fn why(&self, id: &str) -> Result<Record, String> {
        let json = self.run(&["why", id, "--json"])?;
        serde_json::from_str(&json).map_err(|e| format!("proj why {id} doesn't read: {e}"))
    }

    fn stop(&self, id: &str) -> Result<(), String> {
        self.run(&["stop", id]).map(drop)
    }

    fn answers(&self, id: &str) -> Result<Option<Vec<u8>>, String> {
        let path = self.0.jobs.join(id).join("answers.json");
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }
}
