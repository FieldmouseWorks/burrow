//! Engine selection, routing, plugin execution, and normalized transcripts.
//!
//! `burrow-proj` holds no closed engine list and no provider-specific execution
//! or stream-parsing logic. Every engine is identified by its configured plugin
//! name (`String`) and executed through Conary's `conary-plugin v5` (`job`)
//! protocol ([`plugin`]).

use crate::{config::Config, job::Job};
use serde::{Deserialize, Serialize};
#[cfg(test)]
use std::fs;
#[cfg(test)]
use std::path::Path;

mod capabilities;
mod command;
pub(crate) mod credit;
mod handoff;
mod machine;
pub(crate) mod plugin;
mod route;
mod stream;

pub use command::{LONG, NEXT};
pub use handoff::{first_prompt, first_prompt_for};
pub use plugin::{PluginRefusal, RefusalKind, RunResult, Running, SpawnError, exec_plugin, spawn};
pub use stream::{
    Said, answer, is_error, last_result, names_session, record_session, refused_file, result_of,
    results, resume_for, stream_names_session, stream_session, stream_tail,
};

/// Default engine name when legacy records predate explicit engine tracking.
pub const DEFAULT_ENGINE: &str = "claude";

/// What a job's record says when it names Codex, which proj no longer runs:
/// the work is retired, never carried on another engine's model.
pub const RETIRED: &str = "- this job ran on Codex, which is retired: label its issue `claude` to start it again on Claude";

/// Validate an engine plugin name from a job record, CLI flag, or route:
/// rejects retired `codex` with [`RETIRED`] and enforces valid plugin syntax.
pub fn parse_engine(s: &str) -> Result<String, String> {
    if burrow_settings::roles::retired_engine(s) {
        return Err(RETIRED.trim_start_matches("- ").to_owned());
    }
    if burrow_settings::roles::valid_engine_name(s) {
        Ok(s.to_owned())
    } else {
        Err(format!("unknown engine {s}"))
    }
}

pub(crate) fn deserialize_engine<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<String, D::Error> {
    let raw = String::deserialize(d)?;
    parse_engine(&raw).map_err(serde::de::Error::custom)
}

pub(crate) fn deserialize_opt_engine<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<String>, D::Error> {
    let raw = Option::<String>::deserialize(d)?;
    raw.map(|s| parse_engine(&s))
        .transpose()
        .map_err(serde::de::Error::custom)
}

/// What one turn runs on: the engine plugin its role gives it, and the model
/// and effort the settings give that role on that engine. None leaves the
/// plugin's own setting.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    #[serde(deserialize_with = "deserialize_engine")]
    pub engine: String,
    pub model: Option<String>,
    pub effort: Option<String>,
}

impl Default for Choice {
    fn default() -> Self {
        Self {
            engine: DEFAULT_ENGINE.to_owned(),
            model: None,
            effort: None,
        }
    }
}

/// Inbox jobs pin their label's engine; role jobs resolve the settings each turn.
/// Both read the job's protected request ([`crate::job::Request`]), never
/// the files a job may write.
pub fn choice(cfg: &Config, job: &Job) -> Result<Choice, String> {
    if let Some(n) = job.turns()
        && let Some(identity) = crate::retry::Identity::load(job, &n)?
    {
        return Ok(identity.choice);
    }
    let request = crate::job::Request::load(job)?;
    requested(cfg, job, &request, request.engine.as_deref())
}

/// Re-resolve a waiting turn against live host settings so a queued turn
/// never stays stuck behind a held engine after live role settings move it.
/// Reads only host-owned [`crate::job::Request`] and `settings.roles`.
pub(crate) fn refresh_queued(cfg: &Config, job: &Job, n: &str) -> Result<Option<Choice>, String> {
    if cfg.settings.is_none() {
        return Ok(None);
    }
    let Ok(request) = crate::job::Request::load(job) else {
        return Ok(None);
    };
    let live = requested(cfg, job, &request, request.engine.as_deref())?;
    let saved = crate::retry::Identity::load(job, n)?;
    if let Some(saved) = saved {
        if saved.choice.engine == live.engine {
            return Ok(Some(saved.choice));
        }
        crate::retry::Identity::reroute(job, n, &live)?;
    } else {
        crate::retry::Identity::capture_choice(job, n, &live)?;
    }
    let _ = job.record_choice(n, &live);
    crate::snapshot::stream::notify(cfg);
    Ok(Some(live))
}

/// The request's role, scoped to the repository its admission bound (not
/// its project's origin, which the job's git metadata grant reaches), with
/// `pinned` for the engine, in the sandbox the request names or here.
pub(crate) fn requested(
    cfg: &Config,
    job: &Job,
    request: &crate::job::Request,
    pinned: Option<&str>,
) -> Result<Choice, String> {
    let role = request.role.as_deref().unwrap_or_default();
    let repo = if role.is_empty() {
        None
    } else {
        crate::policy::repository(job)
    };
    // A sandbox has its engine's login and hosts alone: its turns run on
    // that engine, whatever the route says now.
    if request.agent.is_some()
        && let Some(engine) = request.agent_engine.as_deref()
    {
        let tuning = cfg.role_settings()?.tuning(role, repo.as_deref(), engine);
        return Ok(Choice {
            engine: engine.to_owned(),
            model: tuning.model,
            effort: tuning.effort,
        });
    }
    resolve(cfg, role, repo.as_deref(), pinned, request.agent.is_some())
}

/// The engine a new job's sandbox is made for: where its role's route, for
/// the repository it's admitted for, runs a sandboxed turn now.
pub(crate) fn for_sandbox(
    cfg: &Config,
    request: &crate::job::Request,
    identity: &crate::policy::Identity,
) -> Result<String, String> {
    let role = request.role.as_deref().unwrap_or_default();
    let repo = match identity {
        crate::policy::Identity::Repository(repo) if !role.is_empty() => Some(repo.as_str()),
        _ => None,
    };
    resolve(cfg, role, repo, request.engine.as_deref(), true).map(|c| c.engine)
}

/// Turn startup, a refused run's retry and drain admission share role
/// scopes, pins and routes: the engine is where the role's route runs a
/// turn now ([`route`]), from the pinned engine on when the route names it,
/// and the model and effort follow the engine it ends on.
pub(crate) fn resolve(
    cfg: &Config,
    role: &str,
    repo: Option<&str>,
    pinned: Option<&str>,
    sandboxed: bool,
) -> Result<Choice, String> {
    // One version of the settings: the engine and its tuning can't come
    // from either side of an `engines use`.
    let settings = cfg.role_settings()?;
    let pinned = pinned.map(parse_engine).transpose()?;
    let meters = route::Live {
        cfg,
        sandboxed,
        now: crate::now(),
    };
    let engine = route::engine(settings.route(role, repo), pinned.as_deref(), &meters)?;
    let tuning = settings.tuning(role, repo, &engine);
    Ok(Choice {
        engine,
        model: tuning.model,
        effort: tuning.effort,
    })
}

/// Conversation key for any engine name: `claude` and `claude-api` share the
/// `claude` conversation; every other engine has a conversation of its own.
pub fn conversation_of(engine: &str) -> &str {
    match engine {
        "claude" | burrow_settings::credit::ENGINE => "claude",
        other => other,
    }
}

/// Where the job's route runs its turn now when a refusal moves it from `from`
/// to another engine with room. Moves within a shared conversation resume
/// directly; moves across different conversations rebuild the handoff prompt
/// in `turn.rs`.
pub(crate) fn reroute(cfg: &Config, job: &Job, from: &Choice) -> Result<Option<Choice>, String> {
    let request = crate::job::Request::load(job)?;
    let next = requested(cfg, job, &request, request.engine.as_deref())?;
    Ok((next.engine != from.engine).then_some(next))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tests::set;

    /// `claude-api` takes the settings door's routes, models and efforts as
    /// `claude` does, each engine its own; its name survives a record.
    #[test]
    fn the_api_engine_takes_its_own_tuning_through_the_door() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
        set(
            &mut cfg,
            tmp.path(),
            &[
                "offer claude models sub-model",
                "offer claude-api models api-model",
                "model * claude sub-model low",
                "model * claude-api api-model high",
                "role work claude-api",
            ],
        );
        let chosen = resolve(&cfg, "work", None, None, false).unwrap();
        assert_eq!(
            chosen,
            Choice {
                engine: "claude-api".into(),
                model: Some("api-model".into()),
                effort: Some("high".into()),
            }
        );
        assert_eq!(
            resolve(&cfg, "other", None, None, false)
                .unwrap()
                .model
                .as_deref(),
            Some("sub-model")
        );
        let pinned = resolve(&cfg, "work", None, Some("claude"), false).unwrap();
        assert_eq!(pinned.engine, "claude");
        assert_eq!(pinned.model.as_deref(), Some("sub-model"));
        let saved = serde_json::to_string(&chosen).unwrap();
        assert!(saved.contains(r#""engine":"claude-api""#), "{saved}");
        assert_eq!(serde_json::from_str::<Choice>(&saved).unwrap(), chosen);
        assert_eq!(parse_engine("claude-api"), Ok("claude-api".to_owned()));
        assert_eq!(conversation_of("claude-api"), "claude");
    }

    #[test]
    fn arbitrary_plugin_engines_resolve_routes_and_models_through_settings() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
        set(
            &mut cfg,
            tmp.path(),
            &[
                "offer antigravity models ag-m1",
                "offer antigravity efforts low high",
                "offer third-plugin models third-m1",
                "offer third-plugin efforts medium",
                "role triage antigravity",
                "model triage antigravity ag-m1 high",
                "role review third-plugin",
                "model review third-plugin third-m1 medium",
            ],
        );
        assert_eq!(
            resolve(&cfg, "triage", None, None, false).unwrap(),
            Choice {
                engine: "antigravity".into(),
                model: Some("ag-m1".into()),
                effort: Some("high".into()),
            }
        );
        assert_eq!(
            resolve(&cfg, "review", None, None, false).unwrap(),
            Choice {
                engine: "third-plugin".into(),
                model: Some("third-m1".into()),
                effort: Some("medium".into()),
            }
        );
    }

    #[test]
    fn the_planner_runs_on_its_roles_settings_like_any_other() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
        set(&mut cfg, tmp.path(), &[]);
        assert_eq!(
            resolve(&cfg, "planner", None, None, false).unwrap(),
            Choice::default()
        );
        set(
            &mut cfg,
            tmp.path(),
            &[
                "offer claude models selected",
                "model planner claude selected",
            ],
        );
        let chosen = resolve(&cfg, "planner", None, None, false).unwrap();
        assert_eq!(chosen.engine, "claude");
        assert_eq!(chosen.model.as_deref(), Some("selected"));
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
            (c.engine.as_str(), c.model.as_deref(), c.effort.as_deref()),
            ("claude", Some("small"), Some("low"))
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
        let path = burrow_test::manifest_dir!().join("../../modules/autoland/step_jobs.py");
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
    fn a_judge_follows_its_role_or_inbox_override() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
        cfg.jobs = dir.path().join("jobs");
        set(
            &mut cfg,
            dir.path(),
            &[
                "offer claude models chosen",
                "model steward-judged claude chosen max",
            ],
        );
        let job = Job::new(&cfg, "judge");
        fs::create_dir_all(&job.dir).unwrap();
        fs::write(job.path("role"), "steward-judged").unwrap();
        let c = choice(&cfg, &job).unwrap();
        assert_eq!(
            (c.engine.as_str(), c.model.as_deref(), c.effort.as_deref()),
            ("claude", Some("chosen"), Some("max"))
        );
        // Pinned by its starter (an inbox label), not by a file it writes:
        // its model and effort follow its role on the engine it's pinned to.
        let mut request = crate::job::Request::load(&job).unwrap();
        request.engine = Some("claude".into());
        request.save(&job).unwrap();
        fs::write(job.path("engine"), "other").unwrap();
        let job = Job::new(&cfg, "judge");
        assert_eq!(choice(&cfg, &job).unwrap(), c);
        // The recorded choice identifies the engine and model that judged.
        let recorded = serde_json::to_value(&c).unwrap();
        assert_eq!(recorded["engine"], "claude");
        assert_eq!(recorded["model"], "chosen");
    }

    #[test]
    fn a_job_whose_record_names_codex_is_retired_not_run_on_claude() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
        cfg.jobs = dir.path().join("jobs");
        let job = Job::new(&cfg, "old");
        fs::create_dir_all(&job.dir).unwrap();
        crate::ask::ensure(&job).unwrap();
        // Its protected request, as a proj that ran Codex recorded it.
        fs::write(
            crate::ask::host_path(&job, "request"),
            r#"{"project":"p","engine":"codex","role":null,"app":null,"agent":null,"owned":false}"#,
        )
        .unwrap();
        let err = choice(&cfg, &job).unwrap_err();
        assert!(err.contains("Codex, which is retired"), "{err}");
        // So is a turn's execution identity, which a retained retry restores.
        fs::write(
            crate::ask::host_path(&job, "request"),
            r#"{"project":"p","engine":null,"role":null,"app":null,"agent":null,"owned":false}"#,
        )
        .unwrap();
        job.set_turns(1).unwrap();
        fs::write(
            job.turn_file("1", crate::record::TurnFile::Execution),
            r#"{"choice":{"engine":"codex","model":"m","effort":null}}"#,
        )
        .unwrap();
        let err = choice(&cfg, &job).unwrap_err();
        assert!(err.contains("Codex, which is retired"), "{err}");
    }

    #[test]
    fn burrow_proj_has_no_closed_engine_enum_or_anthropic_crate_dependency() {
        let root = burrow_test::manifest_dir!();
        let cargo_toml = fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(
            !cargo_toml.contains("burrow-engine-anthropic"),
            "burrow-proj must not depend on burrow-engine-anthropic"
        );
        let engine_mod = fs::read_to_string(root.join("src/engine/mod.rs")).unwrap();
        assert!(
            !engine_mod
                .lines()
                .any(|l| l.trim_start().starts_with(concat!("pub enum ", "Engine"))),
            "closed Engine enum must be deleted"
        );
    }
}

#[cfg(test)]
mod scoped_tests {
    use super::*;
    use std::process::Command;

    /// A job its starter made: its request, and the repository its
    /// admission binds from the project's origin as it is now.
    fn started<'a>(
        cfg: &'a Config,
        id: &'a str,
        project: &Path,
        role: &str,
        engine: Option<&str>,
    ) -> Job<'a> {
        let job = Job::new(cfg, id);
        fs::create_dir_all(&job.dir).unwrap();
        crate::job::Request {
            project: project.to_string_lossy().into_owned(),
            engine: engine.map(str::to_owned),
            role: Some(role.into()),
            ..Default::default()
        }
        .save(&job)
        .unwrap();
        job
    }

    /// A sandbox is made for the engine its job's route runs a sandboxed
    /// turn on, and its turns keep to that engine, with its tuning, whatever
    /// the route says later; one made before that was kept follows the route.
    #[test]
    fn a_sandboxed_job_keeps_to_the_engine_its_sandbox_was_made_for() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
        cfg.jobs = dir.path().join("jobs");
        crate::config::tests::set(
            &mut cfg,
            dir.path(),
            &[
                "offer claude-api models api-model",
                "model * claude-api api-model high",
                "role work claude",
            ],
        );
        let job = started(&cfg, "vm-job", dir.path(), "work", None);
        let mut request = crate::job::Request::load(&job).unwrap();
        assert_eq!(
            for_sandbox(&cfg, &request, &crate::policy::Identity::Local).unwrap(),
            "claude"
        );
        request.agent = Some("work-1".into());
        request.save(&job).unwrap();
        assert_eq!(choice(&cfg, &job).unwrap().engine, "claude");
        request.agent_engine = Some("claude-api".into());
        request.save(&job).unwrap();
        assert_eq!(
            choice(&cfg, &job).unwrap(),
            Choice {
                engine: "claude-api".into(),
                model: Some("api-model".into()),
                effort: Some("high".into()),
            }
        );
    }

    #[test]
    fn scoped_choices_follow_the_bound_origin_for_judges_too() {
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
                "offer claude models bare scoped",
                "model steward-handed claude bare",
                "model steward-handed@sam/code claude scoped",
                "model steward-judged@sam/code claude scoped",
            ],
        );
        for (id, url) in [
            ("https", "https://github.com/Sam/Code.git"),
            ("scp", "git@github.com:SAM/CODE.git"),
            ("ssh", "ssh://git@github.com/Sam/Code"),
        ] {
            git(&["remote", "set-url", "origin", url]);
            let job = started(&cfg, id, &project, "steward-handed", None);
            assert_eq!(choice(&cfg, &job).unwrap().model.as_deref(), Some("scoped"));
        }
        let job = started(&cfg, "judged", &project, "steward-judged", None);
        assert_eq!(choice(&cfg, &job).unwrap().model.as_deref(), Some("scoped"));
        // A scoped role pinned to its engine takes its repo's line too.
        let job = started(&cfg, "pinned", &project, "steward-handed", Some("claude"));
        assert_eq!(choice(&cfg, &job).unwrap().model.as_deref(), Some("scoped"));
        for (id, url) in [
            ("other", "https://github.com/other/code.git"),
            ("gitlab", "https://gitlab.com/sam/code.git"),
            ("clone", "/tmp/clone"),
        ] {
            git(&["remote", "set-url", "origin", url]);
            let job = started(&cfg, id, &project, "steward-handed", None);
            assert_eq!(choice(&cfg, &job).unwrap().model.as_deref(), Some("bare"));
        }
        git(&["remote", "remove", "origin"]);
        let job = started(&cfg, "none", &project, "steward-handed", None);
        assert_eq!(choice(&cfg, &job).unwrap().model.as_deref(), Some("bare"));
        let job = started(&cfg, "nowhere", Path::new(""), "steward-handed", None);
        assert_eq!(choice(&cfg, &job).unwrap().model.as_deref(), Some("bare"));
    }
}

#[cfg(test)]
#[path = "request_tests.rs"]
mod request_tests;
