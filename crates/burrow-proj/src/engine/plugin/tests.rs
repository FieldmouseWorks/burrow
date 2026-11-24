use std::fs;
use std::path::{Path, PathBuf};

use conary_core::plugin::{
    Answer, CONTRACTS, Capabilities, Executed, Identity, Kind, Model, Next, Response, Sandbox,
};
use serde_json::Value;
use tempfile::TempDir;

use super::*;
use crate::config::{Config, tests::example};
use crate::job::Job;
use crate::record::{JobFile, TurnFile};

mod runs;

fn setup(tmp: &TempDir) -> (Config, PathBuf) {
    let mut cfg: Config = serde_json::from_value(example()).unwrap();
    cfg.projects = tmp.path().join("projects");
    cfg.host_state = Some(tmp.path().join("host-state"));
    cfg.jobs = tmp.path().join("jobs");
    cfg.scratch = tmp.path().join("scratch");
    cfg.tools.bash = "bash".into();
    cfg.tools.timeout = "timeout".into();
    // Point `me` at a tiny wrapper that implements `proj _plugin <path>` so
    // unit tests execute plugins directly without invoking the test harness as
    // a CLI binary.
    let wrapper = tmp.path().join("proj-self");
    burrow_test::executable(
        &wrapper,
        "#!/bin/sh\nset -euo pipefail\nif [ \"$1\" = \"_plugin\" ]; then\n  shift\n  exec \"$1\"\nfi\nexit 2\n",
    );
    cfg.me = wrapper;
    let project = cfg.projects.join("demo");
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(&cfg.jobs).unwrap();
    fs::create_dir_all(&cfg.scratch).unwrap();
    (cfg, project)
}

fn make_job<'a>(cfg: &'a Config, project: &Path, id: &'a str) -> Job<'a> {
    let job = Job::new(cfg, id);
    fs::create_dir_all(&job.dir).unwrap();
    fs::write(
        job.file(JobFile::Project),
        format!("{}\n", project.display()),
    )
    .unwrap();
    job.set_turns(1).unwrap();
    fs::write(job.turn_file("1", TurnFile::Jsonl), "").unwrap();
    job
}

/// What a plugin that runs on the host declares (`Sandbox::None`).
fn describe_json(engine: &str, models: Vec<Model>, meters: Vec<String>) -> String {
    describe_json_with(engine, models, meters, Sandbox::None)
}

fn describe_json_with(
    engine: &str,
    models: Vec<Model>,
    meters: Vec<String>,
    sandbox: Sandbox,
) -> String {
    let response = Response {
        contract: CONTRACT_V1.into(),
        by: Identity {
            engine: engine.into(),
            session: None,
        },
        answer: Answer::Describe {
            versions: CONTRACTS.iter().map(|c| (*c).into()).collect(),
            kinds: vec![],
            v2_kinds: vec![Kind::Job],
            v4_kinds: vec![],
            models,
            capabilities: Some(Capabilities {
                network: true,
                mcp: false,
                devices: vec![],
                sandbox,
            }),
            meters,
        },
        execution: None,
    };
    serde_json::to_string(&response).unwrap()
}

fn job_json(
    engine: &str,
    session: &str,
    context: Context,
    model: Option<&str>,
    effort: Option<&str>,
    summary: &str,
    next: Next,
) -> String {
    let response = Response {
        contract: CONTRACTS.last().copied().unwrap().into(),
        by: Identity {
            engine: engine.into(),
            session: Some(session.into()),
        },
        answer: Answer::Job {
            summary: summary.into(),
            next,
        },
        execution: Some(Executed {
            model: model.map(str::to_owned),
            effort: effort.map(str::to_owned),
            context,
        }),
    };
    serde_json::to_string(&response).unwrap()
}

/// A third-party Conary v5 (`job`) plugin (no Anthropic code, no hardcoded
/// engine enum) negotiates `describe`, runs fresh and continued turns, emits
/// normalized usage and `Next::Ask`, and persists its engine-scoped session.
#[test]
fn third_party_conary_v5_plugin_runs_fresh_continue_usage_and_next_ask() {
    let tmp = TempDir::new().unwrap();
    let (mut cfg, project) = setup(&tmp);
    let capture = tmp.path().join("captured-requests.jsonl");
    let desc = describe_json(
        "custom-engine",
        vec![Model {
            id: "synthetic-a".into(),
            efforts: vec!["low".into(), "high".into()],
            context_tokens: None,
            max_output_tokens: None,
        }],
        vec!["quota".into()],
    );
    let turn1 = job_json(
        "custom-engine",
        "sess-custom-1",
        Context::Fresh,
        Some("synthetic-a"),
        Some("high"),
        "First turn answer",
        Next::Ask {
            default: "keep plan A".into(),
            one_way: false,
            question: "Continue with plan B?".into(),
        },
    );
    let turn2 = job_json(
        "custom-engine",
        "sess-custom-1",
        Context::Continue {
            session: "sess-custom-1".into(),
        },
        Some("synthetic-a"),
        Some("high"),
        "Continued from sess-custom-1",
        Next::Done,
    );
    let desc_path = tmp.path().join("desc.json");
    let turn1_path = tmp.path().join("turn1.json");
    let turn2_path = tmp.path().join("turn2.json");
    fs::write(&desc_path, &desc).unwrap();
    fs::write(&turn1_path, &turn1).unwrap();
    fs::write(&turn2_path, &turn2).unwrap();

    let plugin = tmp.path().join("custom-engine-plugin");
    let script = format!(
        r#"#!/bin/sh
set -euo pipefail
req="$(cat)"
if printf '%s' "$req" | grep -q '"kind":"describe"'; then
  cat {desc}
  exit 0
fi
printf '%s\n' "$req" >> {capture}
echo '{{"event":"session","session_id":"sess-custom-1"}}' >&2
echo '{{"event":"activity"}}' >&2
echo '{{"event":"usage","session":"sess-custom-1","usage":{{"input_tokens":120,"output_tokens":45,"total_cost_usd":0.12}}}}' >&2
if printf '%s' "$req" | grep -q '"kind":"continue"'; then
  cat {turn2}
else
  cat {turn1}
fi
"#,
        desc = desc_path.display(),
        capture = capture.display(),
        turn1 = turn1_path.display(),
        turn2 = turn2_path.display(),
    );
    burrow_test::executable(&plugin, &script);
    cfg.plugins.insert("custom-engine".into(), plugin);

    let job = make_job(&cfg, &project, "job-custom");
    let waitdir = job.turn_file("1", TurnFile::Waits);
    fs::create_dir_all(&waitdir).unwrap();
    let prompt1 = job.prompt("1");
    fs::write(&prompt1, "Solve task one\n").unwrap();

    let choice = Choice {
        engine: "custom-engine".into(),
        model: Some("synthetic-a".into()),
        effort: Some("high".into()),
    };

    // Turn 1: fresh context -> Next::Ask + usage + session capture.
    let mut running = spawn(&cfg, &job, "1", &prompt1, &waitdir, None, true, "", &choice).unwrap();
    let status = running.child.take().unwrap().wait().unwrap();
    let res = running.finish(status, false, &cfg, &job, "1", 0);
    assert_eq!(res.rc, 0);
    assert!(res.refusal.is_none());
    let last = res.last.expect("result recorded in 1.jsonl");
    assert_eq!(last["is_error"], false);
    assert_eq!(last["result"], "First turn answer");
    assert_eq!(last["usage"]["input_tokens"], 120);
    assert_eq!(last["usage"]["output_tokens"], 45);
    assert_eq!(
        stream::resume_for(&job, "custom-engine").as_deref(),
        Some("sess-custom-1")
    );
    assert_eq!(
        crate::read(&job.file(JobFile::EndNext)).as_deref(),
        Some("ask")
    );
    // The owner's question leads with the question the plugin put, not
    // only the turn's summary (`N.result`, as the turn writes it).
    fs::write(job.turn_file("1", TurnFile::Result), "First turn answer\n").unwrap();
    crate::ask::finish(&cfg, &job, "ask").unwrap();
    assert_eq!(
        fs::read_to_string(crate::ask::host_path(&job, "question-text")).unwrap(),
        "Continue with plan B?\n\nFirst turn answer"
    );

    // Turn 2: continuation context passes previous session ID.
    job.set_turns(2).unwrap();
    fs::write(job.turn_file("2", TurnFile::Jsonl), "").unwrap();
    let prompt2 = job.prompt("2");
    fs::write(&prompt2, "Proceed\n").unwrap();
    let mut running2 = spawn(
        &cfg, &job, "2", &prompt2, &waitdir, None, false, "", &choice,
    )
    .unwrap();
    let status2 = running2.child.take().unwrap().wait().unwrap();
    let res2 = running2.finish(status2, false, &cfg, &job, "2", 0);
    assert_eq!(res2.rc, 0);
    assert_eq!(res2.last.unwrap()["result"], "Continued from sess-custom-1");

    // Verify both requests sent valid v5 JSON with expected contexts.
    let lines: Vec<Value> = fs::read_to_string(&capture)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["contract"], *CONTRACTS.last().unwrap());
    assert_eq!(lines[0]["execution"]["context"]["kind"], "fresh");
    assert_eq!(lines[1]["execution"]["context"]["kind"], "continue");
    assert_eq!(lines[1]["execution"]["context"]["session"], "sess-custom-1");
}

/// A plugin whose `describe` identity or execution provenance contradicts the
/// configured engine or negotiated model is rejected by the runner.
#[test]
fn plugin_rejects_mismatched_engine_identity_and_provenance() {
    let tmp = TempDir::new().unwrap();
    let (mut cfg, project) = setup(&tmp);

    // Case 1: `describe` returns wrong `by.engine`.
    let bad_desc = tmp.path().join("bad-desc.json");
    fs::write(&bad_desc, describe_json("someone-else", vec![], vec![])).unwrap();
    let bad_id = tmp.path().join("bad-id-plugin");
    burrow_test::executable(&bad_id, format!("#!/bin/sh\ncat {}\n", bad_desc.display()));
    cfg.plugins.insert("expected-engine".into(), bad_id);
    let err = describe(&cfg, &cfg.plugins["expected-engine"], "expected-engine").unwrap_err();
    assert!(err.contains("reported engine identity"), "{err}");

    // Case 2: `job` returns executed model different from negotiated model.
    let prov_desc = tmp.path().join("prov-desc.json");
    let prov_resp = tmp.path().join("prov-resp.json");
    fs::write(
        &prov_desc,
        describe_json(
            "prov-engine",
            vec![Model {
                id: "model-a".into(),
                efforts: vec!["low".into()],
                context_tokens: None,
                max_output_tokens: None,
            }],
            vec![],
        ),
    )
    .unwrap();
    fs::write(
        &prov_resp,
        job_json(
            "prov-engine",
            "s1",
            Context::Fresh,
            Some("model-b"),
            Some("low"),
            "spoofed",
            Next::Done,
        ),
    )
    .unwrap();
    let bad_prov = tmp.path().join("bad-prov-plugin");
    burrow_test::executable(
        &bad_prov,
        format!(
            "#!/bin/sh\nreq=\"$(cat)\"\nif printf '%s' \"$req\" | grep -q '\"kind\":\"describe\"'; then cat {}; else cat {}; fi\n",
            prov_desc.display(),
            prov_resp.display()
        ),
    );
    cfg.plugins.insert("prov-engine".into(), bad_prov);
    let job = make_job(&cfg, &project, "job-prov");
    let waitdir = job.turn_file("1", TurnFile::Waits);
    fs::create_dir_all(&waitdir).unwrap();
    let prompt = job.prompt("1");
    fs::write(&prompt, "hi\n").unwrap();
    let choice = Choice {
        engine: "prov-engine".into(),
        model: Some("model-a".into()),
        effort: Some("low".into()),
    };
    let mut running = spawn(&cfg, &job, "1", &prompt, &waitdir, None, false, "", &choice).unwrap();
    let status = running.child.take().unwrap().wait().unwrap();
    let res = running.finish(status, false, &cfg, &job, "1", 0);
    assert_ne!(res.rc, 0);
    assert_eq!(res.last, None);
}

/// Structured stderr refusals (`usage_limit`, `login_expired`, `credit_exhausted`)
/// are classified purely from plugin diagnostics, never provider text scraping;
/// pre-start credit refusals leave `N.jsonl` untouched so retries keep prior answers.
#[test]
fn structured_stderr_refusals_classify_without_provider_strings() {
    let tmp = TempDir::new().unwrap();
    let (mut cfg, project) = setup(&tmp);
    let desc_path = tmp.path().join("metered-desc.json");
    fs::write(
        &desc_path,
        describe_json("claude-api", vec![], vec!["credit".into()]),
    )
    .unwrap();
    let plugin = tmp.path().join("refusing-plugin");
    burrow_test::executable(
        &plugin,
        format!(
            r#"#!/bin/sh
set -euo pipefail
req="$(cat)"
if printf '%s' "$req" | grep -q '"kind":"describe"'; then
  cat {desc}
  exit 0
fi
case "$req" in
  *trigger\ credit*)
    echo '{{"event":"refusal","refusal":{{"kind":"credit_exhausted","reason":"credit cap reached"}}}}' >&2
    exit 2
    ;;
  *trigger\ quota*)
    echo '{{"event":"stream","line":"{{\"type\":\"result\",\"is_error\":true,\"result\":\"quota reached\"}}"}}' >&2
    echo '{{"event":"refusal","refusal":{{"kind":"usage_limit","reason":"quota reached","resets_at":1900000000}}}}' >&2
    exit 2
    ;;
  *)
    echo '{{"event":"refusal","refusal":{{"kind":"login_expired","reason":"re-authenticate"}}}}' >&2
    exit 3
    ;;
esac
"#,
            desc = desc_path.display()
        ),
    );
    cfg.plugins.insert("claude-api".into(), plugin);

    let job = make_job(&cfg, &project, "job-refuse");
    let waitdir = job.turn_file("1", TurnFile::Waits);
    fs::create_dir_all(&waitdir).unwrap();
    let choice = Choice {
        engine: "claude-api".into(),
        model: None,
        effort: None,
    };

    // Pre-start credit refusal leaves 1.jsonl at offset 0 (no result line written).
    let p_credit = job.prompt("1");
    fs::write(&p_credit, "trigger credit\n").unwrap();
    let mut r1 = spawn(
        &cfg, &job, "1", &p_credit, &waitdir, None, false, "", &choice,
    )
    .unwrap();
    let s1 = r1.child.take().unwrap().wait().unwrap();
    let out1 = r1.finish(s1, false, &cfg, &job, "1", 0);
    assert_eq!(out1.rc, 2);
    assert_eq!(out1.last, None);
    assert_eq!(
        out1.refusal,
        Some(PluginRefusal {
            kind: RefusalKind::CreditExhausted,
            reason: "credit cap reached".into(),
            resets_at: None,
        })
    );
    assert_eq!(
        fs::metadata(job.turn_file("1", TurnFile::Jsonl))
            .unwrap()
            .len(),
        0
    );

    // Post-start quota refusal records normalized `burrow.refusal` for deploy overlap.
    let p_quota = job.turn_file("1", TurnFile::Killed);
    fs::write(&p_quota, "trigger quota\n").unwrap();
    let mut r2 = spawn(
        &cfg, &job, "1", &p_quota, &waitdir, None, false, "", &choice,
    )
    .unwrap();
    let s2 = r2.child.take().unwrap().wait().unwrap();
    let out2 = r2.finish(s2, false, &cfg, &job, "1", 0);
    assert_eq!(
        out2.refusal,
        Some(PluginRefusal {
            kind: RefusalKind::UsageLimit,
            reason: "quota reached".into(),
            resets_at: Some(1_900_000_000),
        })
    );
    assert_eq!(
        stream::refused_file(&job.turn_file("1", TurnFile::Jsonl), 0),
        Some((Some(1_900_000_000), "quota reached".into()))
    );

    // Login refusal classifies as LoginExpired.
    let from3 = fs::metadata(job.turn_file("1", TurnFile::Jsonl))
        .unwrap()
        .len();
    let p_login = job.turn_file("1", TurnFile::Cut);
    fs::write(&p_login, "trigger login\n").unwrap();
    let mut r3 = spawn(
        &cfg, &job, "1", &p_login, &waitdir, None, false, "", &choice,
    )
    .unwrap();
    let s3 = r3.child.take().unwrap().wait().unwrap();
    let out3 = r3.finish(s3, false, &cfg, &job, "1", from3);
    assert_eq!(out3.rc, 3);
    assert_eq!(
        out3.refusal,
        Some(PluginRefusal {
            kind: RefusalKind::LoginExpired,
            reason: "re-authenticate".into(),
            resets_at: None,
        })
    );
}

/// Stand-ins for `timeout(1)` and `agent`: `agent sh ID COMMAND...` runs
/// COMMAND here, as the sandbox ID, after recording each `agent` call; any
/// other command `timeout` is given runs as it is.
fn stand_in_sandbox(tmp: &TempDir, cfg: &mut Config) -> (PathBuf, PathBuf) {
    let calls = tmp.path().join("agent-calls");
    let agent = tmp.path().join("agent");
    burrow_test::executable(
        &agent,
        format!(
            r#"#!/bin/sh
set -euo pipefail
printf '%s\n' "$*" >> {calls}
case "$1" in
  sh) shift 2; exec "$@" ;;
  stop) exit 0 ;;
esac
exit 97
"#,
            calls = calls.display()
        ),
    );
    let timeout = tmp.path().join("timeout");
    burrow_test::executable(
        &timeout,
        format!(
            r#"#!/bin/sh
set -euo pipefail
shift 3
if [ "$1" = agent ]; then shift; exec {agent} "$@"; fi
exec "$@"
"#,
            agent = agent.display()
        ),
    );
    cfg.tools.timeout = timeout;
    (agent, calls)
}

/// A `--vm` job's turn runs its engine's plugin inside the job's own
/// sandbox (`agent sh ID`), with the turn's instructions and the place the
/// guest's `proj` declares the next step, a stale declaration removed first;
/// what the plugin declares there is what the host collects into the job.
#[test]
fn sandboxed_turn_runs_its_plugin_in_the_jobs_sandbox_with_the_turns_context() {
    let tmp = TempDir::new().unwrap();
    let (mut cfg, project) = setup(&tmp);
    let (agent, calls) = stand_in_sandbox(&tmp, &mut cfg);
    let seen = tmp.path().join("seen-env");
    let desc_path = tmp.path().join("desc.json");
    let turn_path = tmp.path().join("turn.json");
    fs::write(
        &desc_path,
        describe_json_with("vm-engine", vec![], vec![], Sandbox::Needs),
    )
    .unwrap();
    fs::write(
        &turn_path,
        job_json(
            "vm-engine",
            "sess-vm-1",
            Context::Fresh,
            None,
            None,
            "answered in the sandbox",
            Next::Done,
        ),
    )
    .unwrap();
    // As the guest's `proj done` does: the declaration in PROJ_VM_NEXT.
    let plugin = tmp.path().join("vm-engine-plugin");
    burrow_test::executable(
        &plugin,
        format!(
            r#"#!/bin/sh
set -euo pipefail
req="$(cat)"
if printf '%s' "$req" | grep -q '"kind":"describe"'; then cat {desc}; exit 0; fi
{{
  printf 'next=%s\n' "${{PROJ_VM_NEXT:-}}"
  printf 'stale=%s\n' "$([ -e "${{PROJ_VM_NEXT:-/nonexistent}}" ] && echo yes || echo no)"
  printf 'stream=%s\n' "${{PROJ_STREAM_EVENTS:-}}"
  printf 'prompt=%s\n' "${{PROJ_APPEND_SYSTEM_PROMPT:-}}"
}} > {seen}
printf '{{"end":"done","targets":[]}}' > "$PROJ_VM_NEXT"
cat {turn}
"#,
            desc = desc_path.display(),
            seen = seen.display(),
            turn = turn_path.display(),
        ),
    );
    cfg.plugins.insert("vm-engine".into(), plugin.clone());

    let id = format!("job-vm-{}", std::process::id());
    let job = make_job(&cfg, &project, &id);
    let declared = PathBuf::from(crate::guest::path(&job, "1"));
    fs::write(&declared, r#"{"end":"ask","targets":[]}"#).unwrap();
    let waitdir = job.turn_file("1", TurnFile::Waits);
    fs::create_dir_all(&waitdir).unwrap();
    let prompt = job.prompt("1");
    fs::write(&prompt, "Solve it in the sandbox\n").unwrap();
    let choice = Choice {
        engine: "vm-engine".into(),
        model: None,
        effort: None,
    };

    let mut running = spawn(
        &cfg,
        &job,
        "1",
        &prompt,
        &waitdir,
        Some("demo-4"),
        true,
        "",
        &choice,
    )
    .unwrap();
    let status = running.child.take().unwrap().wait().unwrap();
    let res = running.finish(status, false, &cfg, &job, "1", 0);
    assert_eq!(res.rc, 0);
    assert_eq!(res.last.unwrap()["result"], "answered in the sandbox");

    let agent_calls = fs::read_to_string(&calls).unwrap();
    let first = agent_calls.lines().next().unwrap();
    assert!(
        first.starts_with("sh demo-4 env ") && first.ends_with(&plugin.display().to_string()),
        "the plugin runs in the job's own sandbox: {first}"
    );
    let seen = fs::read_to_string(&seen).unwrap();
    assert!(
        seen.contains(&format!("next={}\n", declared.display())),
        "{seen}"
    );
    assert!(seen.contains("stale=no\n"), "{seen}");
    assert!(seen.contains("stream=1\n"), "{seen}");
    assert!(
        seen.contains(&format!("Before answering, run {}", crate::engine::NEXT)),
        "{seen}"
    );

    // What it declared in the sandbox becomes the job's next step.
    crate::guest::collect_with(
        &cfg,
        &job,
        "1",
        "demo-4",
        &agent.display().to_string(),
        Duration::from_secs(30),
    )
    .unwrap();
    let _ = fs::remove_file(&declared);
    assert_eq!(
        crate::read(&job.file(JobFile::EndNext)).as_deref(),
        Some("done")
    );
}

/// A credit is admitted on the host, through a ledger no sandbox is given:
/// a `--vm` turn on a credit-metered engine is refused before anything runs,
/// so the turn's route takes it on rather than spend the credit unmetered.
#[test]
fn sandboxed_turn_on_a_credit_metered_plugin_is_refused_before_it_runs() {
    let tmp = TempDir::new().unwrap();
    let (mut cfg, project) = setup(&tmp);
    let (_, calls) = stand_in_sandbox(&tmp, &mut cfg);
    let desc_path = tmp.path().join("desc.json");
    fs::write(
        &desc_path,
        describe_json_with("claude-api", vec![], vec!["credit".into()], Sandbox::Needs),
    )
    .unwrap();
    let ran = tmp.path().join("ran");
    let plugin = tmp.path().join("claude-api-plugin");
    burrow_test::executable(
        &plugin,
        format!(
            r#"#!/bin/sh
req="$(cat)"
if printf '%s' "$req" | grep -q '"kind":"describe"'; then cat {desc}; exit 0; fi
touch {ran}
exit 1
"#,
            desc = desc_path.display(),
            ran = ran.display(),
        ),
    );
    cfg.plugins.insert("claude-api".into(), plugin);

    let job = make_job(&cfg, &project, "job-claude-api");
    let waitdir = job.turn_file("1", TurnFile::Waits);
    fs::create_dir_all(&waitdir).unwrap();
    let prompt = job.prompt("1");
    fs::write(&prompt, "Spend\n").unwrap();
    let choice = Choice {
        engine: "claude-api".into(),
        model: None,
        effort: None,
    };

    let refused = match spawn(
        &cfg,
        &job,
        "1",
        &prompt,
        &waitdir,
        Some("demo-4"),
        true,
        "",
        &choice,
    ) {
        Err(SpawnError::Refused(r)) => r,
        Err(SpawnError::Failed(e)) => panic!("refused, not failed: {e}"),
        Ok(_) => panic!("a credit-metered plugin started in a sandbox"),
    };
    assert_eq!(refused.kind, RefusalKind::CreditExhausted);
    assert!(!ran.exists(), "the plugin's job ran");
    assert!(!calls.exists(), "the sandbox was reached");
    assert!(!job.turn_file("1", TurnFile::Started).exists());
    assert_eq!(
        fs::read_to_string(job.turn_file("1", TurnFile::Jsonl)).unwrap(),
        ""
    );
}

/// An engine that declares it needs a sandbox (one that bypasses its own
/// permissions, trusting the VM) never runs on the host: a host turn on it is
/// refused before its job starts, so the turn's route takes it on. The same
/// engine in a sandbox runs (`sandboxed_turn_runs_its_plugin_...`).
#[test]
fn host_turn_on_a_plugin_that_needs_a_sandbox_is_refused_before_it_runs() {
    let tmp = TempDir::new().unwrap();
    let (mut cfg, project) = setup(&tmp);
    let desc_path = tmp.path().join("desc.json");
    fs::write(
        &desc_path,
        describe_json_with("vm-only", vec![], vec![], Sandbox::Needs),
    )
    .unwrap();
    let ran = tmp.path().join("ran");
    let plugin = tmp.path().join("vm-only-plugin");
    burrow_test::executable(
        &plugin,
        format!(
            r#"#!/bin/sh
req="$(cat)"
if printf '%s' "$req" | grep -q '"kind":"describe"'; then cat {desc}; exit 0; fi
touch {ran}
exit 1
"#,
            desc = desc_path.display(),
            ran = ran.display(),
        ),
    );
    cfg.plugins.insert("vm-only".into(), plugin);

    let job = make_job(&cfg, &project, "job-vm-only");
    let waitdir = job.turn_file("1", TurnFile::Waits);
    fs::create_dir_all(&waitdir).unwrap();
    let prompt = job.prompt("1");
    fs::write(&prompt, "Run on the host\n").unwrap();
    let choice = Choice {
        engine: "vm-only".into(),
        model: None,
        effort: None,
    };

    let refused = match spawn(&cfg, &job, "1", &prompt, &waitdir, None, true, "", &choice) {
        Err(SpawnError::Refused(r)) => r,
        Err(SpawnError::Failed(e)) => panic!("refused, not failed: {e}"),
        Ok(_) => panic!("a plugin that needs a sandbox started on the host"),
    };
    assert_eq!(refused.kind, RefusalKind::Misplaced);
    // It moves the turn without holding the engine: the same engine still
    // runs a sandboxed turn.
    assert!(!refused.kind.holds_the_engine());
    assert!(!ran.exists(), "the plugin's job ran");
    assert!(!job.turn_file("1", TurnFile::Started).exists());
    assert_eq!(
        fs::read_to_string(job.turn_file("1", TurnFile::Jsonl)).unwrap(),
        ""
    );
}

/// A plugin that says more on stderr during `describe` than a pipe holds
/// still gets its answer read: stderr drains while stdout is read.
#[test]
fn describe_reads_a_plugin_that_says_more_on_stderr_than_a_pipe_holds() {
    let tmp = TempDir::new().unwrap();
    let (mut cfg, _) = setup(&tmp);
    let desc_path = tmp.path().join("desc.json");
    fs::write(&desc_path, describe_json("chatty", vec![], vec![])).unwrap();
    let plugin = tmp.path().join("chatty-plugin");
    burrow_test::executable(
        &plugin,
        format!(
            "#!/bin/sh\ncat >/dev/null\nhead -c 1000000 /dev/zero | tr '\\0' x >&2\ncat {}\n",
            desc_path.display()
        ),
    );
    cfg.plugins.insert("chatty".into(), plugin);
    let started = std::time::Instant::now();
    let described = describe(&cfg, &cfg.plugins["chatty"], "chatty").unwrap();
    assert_eq!(described.version, 5);
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
}

/// A plugin that streams its own result record without usage, and reports
/// its usage apart, has that usage in the one result its turn's accounting
/// reads; a streamed result's own usage stays as it is.
#[test]
fn reported_usage_goes_into_a_streamed_result_that_carries_none() {
    for (streamed, want_input, want_cost) in [
        (
            r#"{"type":"result","is_error":false,"result":"streamed answer"}"#,
            120,
            0.12,
        ),
        (
            r#"{"type":"result","is_error":false,"result":"streamed answer","usage":{"input_tokens":7,"output_tokens":3},"total_cost_usd":0.5}"#,
            7,
            0.5,
        ),
    ] {
        let tmp = TempDir::new().unwrap();
        let (mut cfg, project) = setup(&tmp);
        let desc_path = tmp.path().join("desc.json");
        fs::write(&desc_path, describe_json("streamer", vec![], vec![])).unwrap();
        let resp_path = tmp.path().join("resp.json");
        fs::write(
            &resp_path,
            job_json(
                "streamer",
                "sess-s",
                Context::Fresh,
                None,
                None,
                "streamed answer",
                Next::Done,
            ),
        )
        .unwrap();
        let said = tmp.path().join("stderr.jsonl");
        fs::write(
            &said,
            format!(
                "{}\n{}\n",
                serde_json::json!({"event": "stream", "line": streamed}),
                serde_json::json!({"event": "usage", "session": "sess-s", "usage": {
                    "input_tokens": 120, "output_tokens": 45, "total_cost_usd": 0.12}}),
            ),
        )
        .unwrap();
        let plugin = tmp.path().join("streamer-plugin");
        burrow_test::executable(
            &plugin,
            format!(
                "#!/bin/sh\nreq=\"$(cat)\"\nif printf '%s' \"$req\" | grep -q '\"kind\":\"describe\"'; then cat {desc}; exit 0; fi\ncat {said} >&2\ncat {resp}\n",
                desc = desc_path.display(),
                said = said.display(),
                resp = resp_path.display(),
            ),
        );
        cfg.plugins.insert("streamer".into(), plugin);
        let job = make_job(&cfg, &project, "job-streamer");
        let waitdir = job.turn_file("1", TurnFile::Waits);
        fs::create_dir_all(&waitdir).unwrap();
        let prompt = job.prompt("1");
        fs::write(&prompt, "Stream\n").unwrap();
        let choice = Choice {
            engine: "streamer".into(),
            model: None,
            effort: None,
        };
        let mut running =
            spawn(&cfg, &job, "1", &prompt, &waitdir, None, true, "", &choice).unwrap();
        let status = running.child.take().unwrap().wait().unwrap();
        let res = running.finish(status, false, &cfg, &job, "1", 0);
        assert_eq!(res.rc, 0);
        let last = res.last.expect("the streamed result");
        assert_eq!(last["result"], "streamed answer");
        assert_eq!(last["usage"]["input_tokens"], want_input, "{last}");
        assert_eq!(last["total_cost_usd"], want_cost, "{last}");
        let stream = fs::read_to_string(job.turn_file("1", TurnFile::Jsonl)).unwrap();
        assert_eq!(
            stream
                .lines()
                .filter(|l| stream::result_of(l).is_some())
                .count(),
            1,
            "{stream}"
        );
    }
}
