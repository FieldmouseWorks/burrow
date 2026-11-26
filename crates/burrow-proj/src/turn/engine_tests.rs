use super::*;
use crate::config::tests::example;

#[test]
fn invalid_roles_fail_with_an_outcome_before_taking_a_slot() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg: Config = serde_json::from_value(example()).unwrap();
    cfg.jobs = dir.path().join("jobs");
    cfg.scratch = dir.path().join("scratch");
    crate::config::tests::set(&mut cfg, dir.path(), &[]);
    let overlay = cfg.settings.as_ref().unwrap().roles_file();
    let job = Job::new(&cfg, "job");
    fs::create_dir_all(&job.dir).unwrap();
    job.set_turns(1).unwrap();
    fs::write(job.path("role"), "work").unwrap();
    fs::write(job.path("agent"), "sandbox").unwrap();
    // An overlay a hand edit broke: the door never writes one. An engine
    // the machine no longer runs is one of those.
    for (engine, expected) in [
        ("unknown", "unknown engine unknown"),
        ("codex", "unknown engine codex"),
    ] {
        let routing = serde_json::json!({"routing": {"roles": {"work": {"engine": engine}}}});
        fs::write(&overlay, routing.to_string()).unwrap();
        assert!(exec(&cfg, "job", "normal").is_err());
        assert!(read(&job.turn("1", "outcome")).unwrap().contains(expected));
        assert!(!job.turn("1", "slot").exists());
    }
}

fn setup_host_plugin_job(
    dir: &tempfile::TempDir,
    plugin_body: &str,
) -> (Config, std::path::PathBuf) {
    let mut cfg: Config = serde_json::from_value(example()).unwrap();
    cfg.projects = dir.path().join("projects");
    cfg.jobs = dir.path().join("jobs");
    cfg.scratch = dir.path().join("scratch");
    cfg.host_state = Some(dir.path().join("host-state"));
    cfg.min_free_memory = 0;
    cfg.kick.clear();
    cfg.tools.bash = "bash".into();
    cfg.tools.flock = "flock".into();
    cfg.tools.systemctl = "true".into();
    cfg.tools.timeout = "timeout".into();
    let wrapper = dir.path().join("proj-self");
    burrow_test::executable(
        &wrapper,
        "#!/bin/sh\nset -euo pipefail\nif [ \"$1\" = \"_plugin\" ]; then\n  shift\n  exec \"$1\"\nfi\nexit 2\n",
    );
    cfg.me = wrapper.clone();
    cfg.deployed = wrapper;
    let project = cfg.projects.join("demo");
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(&cfg.jobs).unwrap();
    fs::create_dir_all(&cfg.scratch).unwrap();
    let plugin = dir.path().join("fake-antigravity");
    burrow_test::executable(&plugin, plugin_body);
    cfg.plugins.insert("antigravity".into(), plugin);
    crate::config::tests::set(&mut cfg, dir.path(), &[]);
    let job = Job::new(&cfg, "job");
    fs::create_dir_all(&job.dir).unwrap();
    fs::write(job.path("project"), format!("{}\n", project.display())).unwrap();
    fs::write(job.path("engine"), "antigravity\n").unwrap();
    fs::write(job.path("owned"), "").unwrap();
    let req = crate::job::Request {
        project: project.to_string_lossy().into_owned(),
        engine: Some("antigravity".into()),
        owned: true,
        ..Default::default()
    };
    req.save(&job).unwrap();
    crate::policy::bind(
        &job,
        crate::policy::Identity::Local,
        crate::policy::Kind::Manual,
    )
    .unwrap();
    crate::record::mark(&job, Marker::Unstick)
        .write("")
        .unwrap();
    fs::write(job.prompt("1"), "Initial task brief\n").unwrap();
    fs::write(job.turn("1", "engine"), "antigravity\n").unwrap();
    fs::write(job.turn("1", "result"), "Finished turn 1 work\n").unwrap();
    fs::write(
        job.turn("1", "jsonl"),
        "{\"type\":\"result\",\"session_id\":\"ag-old-1\",\"result\":\"Finished turn 1 work\"}\n",
    )
    .unwrap();
    engine::record_session(&job, "antigravity", "ag-old-1");
    job.set_turns(2).unwrap();
    fs::write(job.prompt("2"), "Follow up on turn 1\n").unwrap();
    (cfg, dir.path().join("capture.jsonl"))
}

fn describe_antigravity() -> String {
    use conary_core::plugin::{
        Answer, CONTRACT_V1, CONTRACTS, Capabilities, Identity, Kind, Model, Response, Sandbox,
    };
    serde_json::to_string(&Response {
        contract: CONTRACT_V1.into(),
        by: Identity {
            engine: "antigravity".into(),
            session: None,
        },
        answer: Answer::Describe {
            versions: CONTRACTS.iter().map(|c| (*c).into()).collect(),
            kinds: vec![],
            v2_kinds: vec![Kind::Job],
            v4_kinds: vec![],
            models: vec![Model {
                id: "test-model".into(),
                efforts: vec!["high".into()],
                context_tokens: None,
                max_output_tokens: None,
            }],
            capabilities: Some(Capabilities {
                network: true,
                mcp: false,
                devices: vec![],
                sandbox: Sandbox::None,
            }),
            meters: vec![],
        },
        execution: None,
    })
    .unwrap()
}

fn fresh_ok_antigravity(session: &str, summary: &str) -> String {
    use conary_core::plugin::{Answer, CONTRACTS, Context, Executed, Identity, Next, Response};
    serde_json::to_string(&Response {
        contract: CONTRACTS.last().copied().unwrap().into(),
        by: Identity {
            engine: "antigravity".into(),
            session: Some(session.into()),
        },
        answer: Answer::Job {
            summary: summary.into(),
            next: Next::Done,
        },
        execution: Some(Executed {
            model: Some("test-model".into()),
            effort: None,
            context: Context::Fresh,
        }),
    })
    .unwrap()
}

#[test]
fn session_lost_refusal_clears_session_and_retries_turn_with_handoff() {
    let dir = tempfile::tempdir().unwrap();
    let capture = dir.path().join("capture.jsonl");
    let desc = describe_antigravity();
    let fresh_ok = fresh_ok_antigravity("ag-fresh-2", "Recovered turn 2");
    let script = format!(
        r#"#!/bin/sh
set -eu
req=$(cat)
if printf '%s' "$req" | grep -q '"kind":"describe"'; then
  printf '%s\n' '{desc}'
  exit 0
fi
printf '%s\n' "$req" >> "{}"
if printf '%s' "$req" | grep -q '"kind":"continue"'; then
  printf '%s\n' '{{"event":"refusal","engine":"antigravity","session":null,"refusal":{{"kind":"session_lost","reason":"persistent host HOME has no prior session state","resets_at":null}},"usage":null}}' >&2
  exit 1
fi
printf '%s\n' '{fresh_ok}'
"#,
        capture.display(),
    );
    let (cfg, capture) = setup_host_plugin_job(&dir, &script);
    let job = Job::new(&cfg, "job");

    let res = exec(&cfg, "job", "normal");
    assert!(
        res.is_ok(),
        "turn::exec should recover from session_lost: {res:?}"
    );
    assert_eq!(read(&job.turn("2", "outcome")).as_deref(), Some("done"));
    assert_eq!(
        read(&job.turn("2", "result")).as_deref(),
        Some("Recovered turn 2")
    );
    assert!(
        job.turn("2", "handoff").exists(),
        "2.handoff should be written"
    );
    assert_eq!(
        engine::resume_for(&job, "antigravity").as_deref(),
        Some("ag-fresh-2")
    );

    let lines: Vec<serde_json::Value> = fs::read_to_string(&capture)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(
        lines.len(),
        2,
        "expected continue attempt then fresh handoff retry"
    );
    assert_eq!(lines[0]["execution"]["context"]["kind"], "continue");
    assert_eq!(lines[0]["execution"]["context"]["session"], "ag-old-1");
    assert_eq!(lines[1]["execution"]["context"]["kind"], "fresh");
    let retry_prompt = lines[1]["prompt"].as_str().unwrap();
    assert!(
        retry_prompt.contains("Initial task brief"),
        "{retry_prompt}"
    );
    assert!(
        retry_prompt.contains("Finished turn 1 work"),
        "{retry_prompt}"
    );
    assert!(
        retry_prompt.contains("Follow up on turn 1"),
        "{retry_prompt}"
    );
}

#[test]
fn repeated_session_lost_refusal_stops_without_looping() {
    let dir = tempfile::tempdir().unwrap();
    let capture = dir.path().join("capture.jsonl");
    let desc = describe_antigravity();
    let script = format!(
        r#"#!/bin/sh
set -eu
req=$(cat)
if printf '%s' "$req" | grep -q '"kind":"describe"'; then
  printf '%s\n' '{desc}'
  exit 0
fi
printf '%s\n' "$req" >> "{}"
printf '%s\n' '{{"event":"refusal","engine":"antigravity","session":null,"refusal":{{"kind":"session_lost","reason":"session vanished again","resets_at":null}},"usage":null}}' >&2
exit 1
"#,
        capture.display(),
    );
    let (cfg, capture) = setup_host_plugin_job(&dir, &script);
    let job = Job::new(&cfg, "job");

    assert!(exec(&cfg, "job", "normal").is_err());
    let calls = fs::read_to_string(&capture).unwrap().lines().count();
    assert_eq!(calls, 2, "should retry at most once with a fresh handoff");
    assert!(
        read(&job.turn("2", "outcome"))
            .unwrap_or_default()
            .contains("session vanished again")
    );
}
