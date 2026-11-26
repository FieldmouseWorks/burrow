use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use burrow_engine_antigravity::config::{Config, CredentialMode, EFFORTS, ENGINE, ModelSpec};
use conary_core::plugin::{CONTRACT, Context, Execution, Identity, Negotiated, Request, Task};
use serde_json::Value;
use tempfile::TempDir;

fn write_executable(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    burrow_test::executable(&path, body);
    path
}

fn sample_models() -> Vec<ModelSpec> {
    vec![ModelSpec {
        id: "test-model".to_owned(),
        efforts: EFFORTS.iter().map(|e| (*e).to_owned()).collect(),
        context_tokens: Some(200_000),
        max_output_tokens: Some(16_384),
    }]
}

fn run_continue(
    cfg_path: &Path,
    cfg: &Config,
    workdir: &Path,
    session: &str,
    configure_env: impl FnOnce(&mut Command),
) -> Output {
    let req = Request {
        contract: CONTRACT.to_owned(),
        execution: Some(Execution {
            choice: None,
            context: Context::Continue {
                session: session.to_owned(),
            },
        }),
        negotiated: Some(Negotiated {
            by: Identity {
                engine: ENGINE.to_owned(),
                session: None,
            },
            offers: cfg.offers(),
        }),
        task: Task::Job {
            repo: "owner/demo".to_owned(),
            workdir: workdir.to_string_lossy().into_owned(),
            prompt: "continue work".to_owned(),
            model: None,
            effort: None,
        },
    };
    let req_bytes = serde_json::to_vec(&req).unwrap();
    let mut cmd = Command::new(burrow_test::program!("burrow-engine-antigravity"));
    cmd.arg(cfg_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_env(&mut cmd);
    let mut child = cmd.spawn().unwrap();
    child.stdin.as_mut().unwrap().write_all(&req_bytes).unwrap();
    drop(child.stdin.take());
    child.wait_with_output().unwrap()
}

fn refusal_event(stderr: &[u8]) -> Option<Value> {
    String::from_utf8_lossy(stderr)
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .find(|v| v.get("event").and_then(Value::as_str) == Some("refusal"))
}

#[test]
fn host_mode_continue_refuses_session_lost_without_running_cli_when_home_missing_or_empty() {
    let dir = TempDir::new().unwrap();
    let ran_marker = dir.path().join("cli-ran");
    let fixture = write_executable(
        dir.path(),
        "ag-should-not-run",
        &format!(
            "#!/bin/sh\necho ran > \"{}\"\nprintf '{{\"type\":\"result\",\"conversation_id\":\"ag-new\",\"result\":\"unexpected\"}}\\n'\n",
            ran_marker.display()
        ),
    );
    let cfg = Config {
        binary: fixture,
        models: sample_models(),
        default_model: Some("test-model".to_owned()),
        credential_mode: CredentialMode::Env,
        credential_env: None,
        target_credential_env: None,
        host_mode: true,
        ..Config::default()
    };
    let cfg_path = dir.path().join("config.json");
    fs::write(&cfg_path, serde_json::to_vec(&cfg).unwrap()).unwrap();

    let missing_home = dir.path().join("does-not-exist");
    let empty_home = dir.path().join("empty-home");
    fs::create_dir_all(&empty_home).unwrap();

    for (label, engine_home) in [
        ("unset", None),
        ("missing", Some(missing_home.as_path())),
        ("empty", Some(empty_home.as_path())),
    ] {
        let output = run_continue(&cfg_path, &cfg, dir.path(), "ag-old-1", |cmd| {
            if let Some(home) = engine_home {
                cmd.env("PROJ_ENGINE_HOME", home);
            } else {
                cmd.env_remove("PROJ_ENGINE_HOME");
            }
        });
        assert!(
            !output.status.success(),
            "{label}: expected failure, got success"
        );
        assert!(
            !ran_marker.exists(),
            "{label}: CLI must not run when isolated host HOME has no prior session"
        );
        let event = refusal_event(&output.stderr).unwrap_or_else(|| {
            panic!(
                "{label}: expected refusal event on stderr, got: {}",
                String::from_utf8_lossy(&output.stderr)
            )
        });
        assert_eq!(event["refusal"]["kind"], "session_lost", "{label}: {event}");
        assert!(
            event["session"].is_null(),
            "{label}: session_lost must not report a replacement session: {event}"
        );
    }
}

#[test]
fn changed_continuation_session_emits_session_lost_refusal_with_null_session() {
    let dir = TempDir::new().unwrap();
    let fixture = write_executable(
        dir.path(),
        "ag-new-session",
        "#!/bin/sh\ncat >/dev/null\nprintf '{\"type\":\"result\",\"conversation_id\":\"ag-new-2\",\"result\":\"started fresh\"}\\n'\n",
    );
    let populated_home = dir.path().join("populated-home");
    fs::create_dir_all(&populated_home).unwrap();
    fs::write(populated_home.join("marker"), "prior-state").unwrap();

    for host_mode in [false, true] {
        let cfg = Config {
            binary: fixture.clone(),
            models: sample_models(),
            default_model: Some("test-model".to_owned()),
            credential_mode: CredentialMode::Env,
            credential_env: None,
            target_credential_env: None,
            host_mode,
            ..Config::default()
        };
        let cfg_path = dir.path().join(format!("config-{host_mode}.json"));
        fs::write(&cfg_path, serde_json::to_vec(&cfg).unwrap()).unwrap();

        let output = run_continue(&cfg_path, &cfg, dir.path(), "ag-old-1", |cmd| {
            cmd.env("PROJ_ENGINE_HOME", &populated_home);
        });
        assert!(
            !output.status.success(),
            "host_mode={host_mode}: expected failure"
        );
        let event = refusal_event(&output.stderr).unwrap_or_else(|| {
            panic!(
                "host_mode={host_mode}: expected refusal event on stderr, got: {}",
                String::from_utf8_lossy(&output.stderr)
            )
        });
        assert_eq!(
            event["refusal"]["kind"], "session_lost",
            "host_mode={host_mode}: {event}"
        );
        assert!(
            event["session"].is_null(),
            "host_mode={host_mode}: session_lost must not record replacement session: {event}"
        );
    }
}
