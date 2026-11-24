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

fn setup(tmp: &TempDir) -> (Config, PathBuf) {
    let mut cfg: Config = serde_json::from_value(example()).unwrap();
    cfg.projects = tmp.path().join("projects");
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
        "#!/usr/bin/env bash\nset -euo pipefail\nif [ \"$1\" = \"_plugin\" ]; then\n  shift\n  exec \"$1\"\nfi\nexit 2\n",
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

fn describe_json(engine: &str, models: Vec<Model>, meters: Vec<String>) -> String {
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
                sandbox: Sandbox::Needs,
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
        r#"#!/usr/bin/env bash
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
    burrow_test::executable(
        &bad_id,
        format!("#!/usr/bin/env bash\ncat {}\n", bad_desc.display()),
    );
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
            "#!/usr/bin/env bash\nreq=\"$(cat)\"\nif printf '%s' \"$req\" | grep -q '\"kind\":\"describe\"'; then cat {}; else cat {}; fi\n",
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
        describe_json("metered-engine", vec![], vec!["credit".into()]),
    )
    .unwrap();
    let plugin = tmp.path().join("refusing-plugin");
    burrow_test::executable(
        &plugin,
        format!(
            r#"#!/usr/bin/env bash
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
    cfg.plugins.insert("metered-engine".into(), plugin);

    let job = make_job(&cfg, &project, "job-refuse");
    let waitdir = job.turn_file("1", TurnFile::Waits);
    fs::create_dir_all(&waitdir).unwrap();
    let choice = Choice {
        engine: "metered-engine".into(),
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
