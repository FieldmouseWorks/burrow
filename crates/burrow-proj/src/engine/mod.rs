//! Engines own commands, sessions, streams and refusals; turns own lifecycle.

use crate::{config::Config, job::Job, read};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
#[cfg(test)]
use std::fs;
use std::fs::File;
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
pub(crate) use role::route;
pub use stream::{
    Said, answer, last_result, names_session, results, stream_names_session, stream_tail,
};

#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Deserialize,
    Serialize,
    schemars::JsonSchema,
)]
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
/// effort the settings give that role on that engine. None leaves the
/// engine's own setting.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    pub engine: Engine,
    pub model: Option<String>,
    pub effort: Option<String>,
}

/// Inbox jobs pin their label's engine; role jobs resolve the settings each turn.
pub fn choice(cfg: &Config, job: &Job) -> Result<Choice, String> {
    let role = read(&job.path("role")).unwrap_or_default();
    let repo = if role.is_empty() {
        String::new()
    } else {
        crate::pick::github_repo(cfg, &crate::pick::project(&job.dir))
    };
    if let Some(n) = job.turns()
        && let Some(identity) = crate::retry::Identity::load(job, &n)?
    {
        return Ok(identity.choice);
    }
    let pinned = read(&job.path("engine"));
    resolve(
        cfg,
        &role,
        (!repo.is_empty()).then_some(repo.as_str()),
        pinned.as_deref(),
    )
}

/// Turn startup and drain admission share role scopes, overflow and pins.
/// The model and effort follow the engine it ends on.
pub(crate) fn resolve(
    cfg: &Config,
    role: &str,
    repo: Option<&str>,
    pinned: Option<&str>,
) -> Result<Choice, String> {
    // One version of the settings: the engine and its tuning can't come
    // from either side of an `engines use`.
    let settings = cfg.role_settings()?;
    let mut engine = cfg.engine_in(&settings, role, repo)?;
    if let Some(e) = pinned {
        engine = Engine::parse(e)?;
    }
    let tuning = settings.tuning(role, repo, engine.name());
    Ok(Choice {
        engine,
        model: tuning.model,
        effort: tuning.effort,
    })
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
    use crate::config::tests::set;

    #[test]
    fn planning_is_a_declared_default_with_no_fallback_or_model_restriction() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
        assert_eq!(
            resolve(&cfg, "planner", None, None).unwrap().engine,
            Engine::Codex
        );
        // Live, at the next decision: Claude with no model needs no
        // explicit small model, and the planner's own model and effort win.
        set(&mut cfg, tmp.path(), &["role planner claude"]);
        assert_eq!(
            resolve(&cfg, "planner", None, None).unwrap(),
            Choice {
                engine: Engine::Claude,
                model: None,
                effort: None
            }
        );
        set(
            &mut cfg,
            tmp.path(),
            &[
                "offer claude models selected",
                "model planner claude selected high",
            ],
        );
        let chosen = resolve(&cfg, "planner", None, None).unwrap();
        assert_eq!(chosen.engine, Engine::Claude);
        assert_eq!(chosen.model.as_deref(), Some("selected"));
        assert_eq!(chosen.effort.as_deref(), Some("high"));
        set(&mut cfg, tmp.path(), &["role planner codex"]);
        assert_eq!(
            resolve(&cfg, "planner", None, None).unwrap().engine,
            Engine::Codex
        );
    }

    #[test]
    fn update_backup_and_firmware_stay_on_claude_and_never_spill() {
        let tmp = tempfile::tempdir().unwrap();
        let meters = tmp.path().join("meters");
        fs::create_dir_all(&meters).unwrap();
        // Claude's week is used up and Codex has credits: anything with an
        // overflow would move.
        fs::write(
            meters.join("claude.json"),
            json!({"windows": {"seven_day": {"used": 100.0, "resets_at": 20000}}, "credits": false, "read_at": crate::now()}).to_string(),
        )
        .unwrap();
        fs::write(
            meters.join("codex.json"),
            json!({"windows": {"300m": {"used": 1.0, "resets_at": 20000}}, "credits": true, "read_at": crate::now()}).to_string(),
        )
        .unwrap();
        let mut cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
        cfg.meters = Some(meters);
        // Every other role overflows to Codex, as `*` is told; the
        // deliberate three do not, by their own declared routes.
        set(&mut cfg, tmp.path(), &["role * claude over 50 codex"]);
        assert_eq!(
            resolve(&cfg, "routine", None, None).unwrap().engine,
            Engine::Codex
        );
        for (role, _) in burrow_settings::roles::CLAUDE_ONLY {
            assert_eq!(
                resolve(&cfg, role, None, None).unwrap().engine,
                Engine::Claude,
                "{role}"
            );
            assert_eq!(
                resolve(&cfg, role, Some("sam/box"), None).unwrap().engine,
                Engine::Claude,
                "{role}"
            );
        }
        // Their declared route is the live setting: changing it moves them.
        set(&mut cfg, tmp.path(), &["role update codex"]);
        assert_eq!(
            resolve(&cfg, "update", None, None).unwrap().engine,
            Engine::Codex
        );
    }

    #[test]
    fn an_unspecified_roles_declared_defaults_change_live() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
        assert_eq!(
            resolve(&cfg, "never-heard-of", None, None).unwrap(),
            Choice::default()
        );
        set(
            &mut cfg,
            tmp.path(),
            &[
                "offer codex models small",
                "model * codex small low",
                "role * codex",
            ],
        );
        assert_eq!(
            resolve(&cfg, "never-heard-of", None, None).unwrap(),
            Choice {
                engine: Engine::Codex,
                model: Some("small".into()),
                effort: Some("low".into()),
            }
        );
    }

    #[test]
    fn roles_cannot_be_routed_while_settings_are_unconfigured() {
        let mut cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
        cfg.settings = None;
        let err = resolve(&cfg, "routine", None, None).unwrap_err();
        assert!(err.contains("settings are not configured"), "{err}");
    }

    #[test]
    fn the_next_job_of_a_role_runs_on_the_model_the_door_set() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
        cfg.jobs = dir.path().join("jobs");
        set(&mut cfg, dir.path(), &[]);
        let job = |cfg: &Config, id: &str| {
            let job = Job::new(cfg, id);
            fs::create_dir_all(&job.dir).unwrap();
            fs::write(job.path("role"), "triage").unwrap();
            choice(cfg, &job).unwrap()
        };
        // No model set: the engine's own, as the schema declares it.
        assert_eq!(job(&cfg, "one"), Choice::default());
        set(
            &mut cfg,
            dir.path(),
            &[
                "offer claude models small mid",
                "model triage claude small low",
            ],
        );
        let c = job(&cfg, "two");
        assert_eq!(
            (c.engine, c.model.as_deref(), c.effort.as_deref()),
            (Engine::Claude, Some("small"), Some("low"))
        );
        // One change through the door, and the next job of the role takes it.
        cfg.settings
            .as_ref()
            .unwrap()
            .command(
                &["model", "triage", "claude", "mid"].map(String::from),
                "test",
            )
            .unwrap();
        let c = job(&cfg, "three");
        assert_eq!((c.model.as_deref(), c.effort), (Some("mid"), None));
        // Another role's is its own still.
        let other = Job::new(&cfg, "four");
        fs::create_dir_all(&other.dir).unwrap();
        fs::write(other.path("role"), "fix").unwrap();
        assert_eq!(choice(&cfg, &other).unwrap(), Choice::default());
    }

    #[test]
    fn a_judge_job_starts_fresh_without_a_writer_or_reviewer_session() {
        // Exercise the steward's actual creation path without its service imports.
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../modules/autoland/step_jobs.py");
        let output = std::process::Command::new("python3")
            .args([
                "-c",
                r#"
import pathlib
import sys
import types
from unittest.mock import Mock

for name in ('claims', 'common', 'proofs', 'stages', 'waits', 'step_context'):
    sys.modules[name] = types.ModuleType(name)
common = sys.modules['common']
common.CFG, common.MIN, common.NOW = {}, 60, 123
common.log, common.ts = lambda _: None, lambda value: value
sys.modules['claims'].KINDS = ()
context = sys.modules['step_context']
for name in ('STRIKES', 'STRIKE_LIMIT', 'ON', 'VERB', 'clock', '_HEAD'):
    setattr(context, name, object())
source = pathlib.Path(sys.argv[1]).read_text()

def assert_fresh(source):
    namespace = {}
    exec(compile(source, sys.argv[1], 'exec'), namespace)
    job = namespace['Jobs']()
    job.repo, job.head, job.tag = 'owner/repo', 'head', 'owner/repo#1'
    job.pr = {'createdAt': 'created'}
    job.st = {'handedJob': 'writer-job', 'reviewJob': 'reviewer-job'}
    job.deadline, job.cleared = lambda _: None, lambda _: None
    common.job = Mock(return_value='fresh-judge-job')
    assert job.started('judged', 'judge this head', at='base')
    common.job.assert_called_once_with(
        'owner/repo', 'judge this head', since='created', role='steward-judged')
    assert job.st['judgedJob'] == 'fresh-judge-job'

assert_fresh(source)
# Negative controls: the same assertion must reject either inherited session.
call = 'role=f"steward-{kind}")'
assert source.count(call) == 1
for session in ('writer-session', 'reviewer-session'):
    inherited = source.replace(call, f'role=f"steward-{{kind}}", session={session!r})')
    try:
        assert_fresh(inherited)
    except AssertionError:
        pass
    else:
        raise AssertionError(f'inherited {session} escaped the fresh-session check')
"#,
            ])
            .arg(path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn a_judge_overflows_when_claudes_five_hour_window_is_short() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
        cfg.meters = Some(dir.path().to_owned());
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        for (engine, window, used) in [("claude", "five_hour", 96), ("codex", "300m", 10)] {
            fs::write(
                dir.path().join(format!("{engine}.json")),
                json!({
                    "windows": {window: {"used": used, "resets_at": now + 3600}},
                    "credits": false,
                    "read_at": now,
                })
                .to_string(),
            )
            .unwrap();
        }
        set(
            &mut cfg,
            dir.path(),
            &[
                "offer codex models selected",
                "offer codex efforts high",
                "role steward-judged claude over 95 codex",
                "model steward-judged codex selected high",
            ],
        );
        let chosen = resolve(&cfg, "steward-judged", None, None).unwrap();
        assert_eq!(chosen.engine, Engine::Codex);
        assert_eq!(chosen.model.as_deref(), Some("selected"));
        assert_eq!(chosen.effort.as_deref(), Some("high"));
        // The recorded choice identifies the engine and model that judged.
        let recorded = serde_json::to_value(&chosen).unwrap();
        assert_eq!(recorded["engine"], "codex");
        assert_eq!(recorded["model"], "selected");
        set(&mut cfg, dir.path(), &["role steward-judged claude"]);
        assert_eq!(
            resolve(&cfg, "steward-judged", None, None).unwrap().engine,
            Engine::Claude
        );
    }
    #[test]
    fn a_judge_follows_its_role_or_inbox_override() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
        cfg.jobs = dir.path().join("jobs");
        set(&mut cfg, dir.path(), &["role steward-judged codex"]);
        {
            let job = Job::new(&cfg, "judge");
            fs::create_dir_all(&job.dir).unwrap();
            fs::write(job.path("role"), "steward-judged").unwrap();
            fs::write(job.path("engine"), "codex").unwrap();
            assert_eq!(choice(&cfg, &job).unwrap().engine, Engine::Codex);
        }
        // Model and effort follow the engine the judge runs on.
        set(
            &mut cfg,
            dir.path(),
            &[
                "offer codex models other",
                "offer claude models chosen",
                "model steward-judged codex other low",
                "model steward-judged claude chosen max",
            ],
        );
        let job = Job::new(&cfg, "judge");
        let c = choice(&cfg, &job).unwrap();
        assert_eq!(
            (c.engine, c.model.as_deref(), c.effort.as_deref()),
            (Engine::Codex, Some("other"), Some("low"))
        );
    }
}

#[cfg(test)]
mod scoped_tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn scoped_choices_follow_the_project_origin_for_judges_too() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        let mut cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
        cfg.jobs = dir.path().join("jobs");
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
        crate::config::tests::set(
            &mut cfg,
            dir.path(),
            &[
                "role steward-handed claude",
                "role steward-handed@sam/code codex",
                "role steward-judged@sam/code codex",
                "offer claude models bare scoped",
                "model steward-handed claude bare",
                "model steward-handed@sam/code claude scoped",
            ],
        );

        let job = Job::new(&cfg, "job");
        fs::create_dir_all(&job.dir).unwrap();
        fs::write(job.path("role"), "steward-handed").unwrap();
        fs::write(job.path("project"), project.to_str().unwrap()).unwrap();
        for url in [
            "https://github.com/Sam/Code.git",
            "git@github.com:SAM/CODE.git",
            "ssh://git@github.com/Sam/Code",
        ] {
            git(&["remote", "set-url", "origin", url]);
            assert_eq!(choice(&cfg, &job).unwrap().engine, Engine::Codex);
        }
        fs::write(job.path("role"), "steward-judged").unwrap();
        assert_eq!(choice(&cfg, &job).unwrap().engine, Engine::Codex);
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
