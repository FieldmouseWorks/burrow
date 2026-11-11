//! Engines own commands, sessions, streams and refusals; turns own lifecycle.

use crate::{config::Config, job::Job, read};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fs::{self, File};
use std::io::{Seek, SeekFrom};
use std::path::Path;

mod capabilities;
pub(crate) mod claude;
mod claude_limit;
mod command;
mod handoff;
mod role;
mod sandbox;
mod stream;
pub use command::{LONG, NEXT, exec_claude, exec_codex, spawn};
pub use handoff::first_prompt;
pub(crate) use role::{RoleKey, role};
pub use stream::{Said, answer, last_result, names_session, stream_names_session, stream_tail};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
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
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "claude" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            _ => Err(format!("unknown engine {s}")),
        }
    }
}

/// What one turn runs on: the engine its role gives it, and the model and
/// effort the models file gives that role on that engine. None leaves the
/// engine's own setting.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Choice {
    pub engine: Engine,
    pub model: Option<String>,
    pub effort: Option<String>,
}

/// Inbox jobs pin their label's engine; role jobs resolve the file each turn.
/// Judges always use Claude, so a role or overflow cannot make Codex judge itself.
pub fn choice(cfg: &Config, job: &Job) -> Result<Choice, String> {
    let role = read(&job.path("role")).unwrap_or_default();
    let repo = if role.is_empty() {
        String::new()
    } else {
        crate::pick::github_repo(cfg, &crate::pick::project(&job.dir))
    };
    resolve(
        cfg,
        &role,
        (!repo.is_empty()).then_some(repo.as_str()),
        read(&job.path("engine")).as_deref(),
    )
}

/// Turn startup and drain admission share role scopes, overflow, pins and
/// the judge restriction. The model and effort follow the engine it ends on.
pub(crate) fn resolve(
    cfg: &Config,
    role: &str,
    repo: Option<&str>,
    pinned: Option<&str>,
) -> Result<Choice, String> {
    let mut engine = cfg.engine_default(
        role,
        repo,
        if role == "planner" {
            Engine::Codex
        } else {
            Engine::Claude
        },
    )?;
    if let Some(e) = pinned {
        engine = Engine::parse(e)?;
    }
    if role == "steward-judged" {
        engine = Engine::Claude;
    }
    let choice = cfg.choice(role, repo, engine)?;
    if role == "planner" && choice.engine == Engine::Claude && choice.model.is_none() {
        return Err(
            "the planner role requires an explicit small model on Claude; its default is Codex"
                .into(),
        );
    }
    Ok(choice)
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

/// Refusal metadata is parsed a record at a time, including after retry offsets.
pub fn refused_file(
    engine: Engine,
    last: &Value,
    path: &Path,
    from: u64,
    now: i64,
) -> Option<(Option<i64>, String)> {
    match engine {
        Engine::Claude => claude::refused_records(
            last,
            stream::lines(path, from)
                .ok()?
                .map_while(Result::ok)
                .filter_map(|line| serde_json::from_str::<Value>(&line).ok()),
            now,
        ),
        Engine::Codex => refused(engine, last, "", now),
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
    fn planning_defaults_to_codex_and_never_falls_through_to_the_claude_default() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
        cfg.roles = tmp.path().join("roles");
        cfg.models = tmp.path().join("models");
        assert_eq!(
            resolve(&cfg, "planner", None, None).unwrap().engine,
            Engine::Codex
        );
        fs::write(&cfg.roles, "planner claude\n").unwrap();
        assert!(
            resolve(&cfg, "planner", None, None)
                .unwrap_err()
                .contains("explicit small model")
        );
        fs::write(&cfg.models, "planner claude selected\n").unwrap();
        let chosen = resolve(&cfg, "planner", None, None).unwrap();
        assert_eq!(chosen.engine, Engine::Claude);
        assert_eq!(chosen.model.as_deref(), Some("selected"));
    }
    #[test]
    fn a_judge_never_routes_to_codex_even_with_a_role_or_inbox_override() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
        cfg.jobs = dir.path().join("jobs");
        cfg.roles = dir.path().join("roles");
        cfg.models = dir.path().join("models");
        fs::write(&cfg.roles, "steward-judged codex\n").unwrap();
        let job = Job::new(&cfg, "judge");
        fs::create_dir_all(&job.dir).unwrap();
        fs::write(job.path("role"), "steward-judged").unwrap();
        fs::write(job.path("engine"), "codex").unwrap();
        assert_eq!(choice(&cfg, &job).unwrap(), Choice::default());
        // The judge's own model and effort are Claude's line, not Codex's.
        fs::write(
            &cfg.models,
            "steward-judged codex other low\nsteward-judged claude chosen max\n",
        )
        .unwrap();
        let c = choice(&cfg, &job).unwrap();
        assert_eq!(
            (c.engine, c.model.as_deref(), c.effort.as_deref()),
            (Engine::Claude, Some("chosen"), Some("max"))
        );
    }
}

#[cfg(test)]
mod scoped_tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn scoped_choices_follow_the_project_origin_and_keep_judges_on_claude() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        let mut cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
        cfg.jobs = dir.path().join("jobs");
        cfg.roles = dir.path().join("roles");
        cfg.models = dir.path().join("models");
        cfg.tools.git = "git".into();
        let git = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(&project)
                    .args(args)
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
        };
        fs::create_dir(&project).unwrap();
        git(&["init", "-q"]);
        git(&["remote", "add", "origin", "https://github.com/Sam/Code.git"]);
        let job = Job::new(&cfg, "job");
        fs::create_dir_all(&job.dir).unwrap();
        fs::write(job.path("role"), "steward-handed").unwrap();
        fs::write(job.path("project"), project.to_str().unwrap()).unwrap();
        fs::write(
            &cfg.roles,
            "steward-handed claude\nsteward-handed@sam/code codex\nsteward-judged@sam/code codex\n",
        )
        .unwrap();
        fs::write(
            &cfg.models,
            "steward-handed claude bare\nsteward-handed@sam/code claude scoped\n",
        )
        .unwrap();
        for url in [
            "https://github.com/Sam/Code.git",
            "git@github.com:SAM/CODE.git",
            "ssh://git@github.com/Sam/Code",
        ] {
            git(&["remote", "set-url", "origin", url]);
            assert_eq!(choice(&cfg, &job).unwrap().engine, Engine::Codex);
        }
        fs::write(job.path("role"), "steward-judged").unwrap();
        assert_eq!(choice(&cfg, &job).unwrap(), Choice::default());
        // A scoped role on Claude takes its repo's line.
        fs::write(job.path("engine"), "claude").unwrap();
        fs::write(job.path("role"), "steward-handed").unwrap();
        assert_eq!(choice(&cfg, &job).unwrap().model.as_deref(), Some("scoped"));
        fs::remove_file(job.path("engine")).unwrap();
        for url in [
            "https://github.com/other/code.git",
            "https://gitlab.com/sam/code.git",
            "/tmp/clone",
        ] {
            git(&["remote", "set-url", "origin", url]);
            assert_eq!(choice(&cfg, &job).unwrap().model.as_deref(), Some("bare"));
        }
        git(&["remote", "remove", "origin"]);
        assert_eq!(choice(&cfg, &job).unwrap().model.as_deref(), Some("bare"));
        fs::remove_file(job.path("project")).unwrap();
        assert_eq!(choice(&cfg, &job).unwrap().model.as_deref(), Some("bare"));
    }
}
