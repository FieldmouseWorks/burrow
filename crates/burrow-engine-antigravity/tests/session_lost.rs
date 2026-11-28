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

#[test]
fn streaming_changed_continuation_session_suppresses_replacement_session_event() {
    let dir = TempDir::new().unwrap();
    let fixture = write_executable(
        dir.path(),
        "ag-stream-new-session",
        "#!/bin/sh\ncat >/dev/null\nprintf '{\"event\":\"init\",\"conversation_id\":\"ag-new-2\",\"init\":{\"model\":\"test-model\"}}\\n'\nprintf '{\"event\":\"result\",\"result\":{\"conversation_id\":\"ag-new-2\",\"status\":\"SUCCESS\",\"response\":\"started fresh\"}}\\n'\n",
    );
    let populated_home = dir.path().join("populated-home");
    fs::create_dir_all(&populated_home).unwrap();
    fs::write(populated_home.join("marker"), "prior-state").unwrap();

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

    let output = run_continue(&cfg_path, &cfg, dir.path(), "ag-old-1", |cmd| {
        cmd.env("PROJ_ENGINE_HOME", &populated_home)
            .env("PROJ_STREAM_EVENTS", "1");
    });
    assert!(!output.status.success(), "expected session_lost failure");

    let events: Vec<Value> = String::from_utf8_lossy(&output.stderr)
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .collect();
    assert!(
        !events
            .iter()
            .any(|v| v.get("event").and_then(Value::as_str) == Some("session")),
        "streaming continuation mismatch must not emit a replacement session event: {events:?}"
    );
    assert!(
        !events.iter().any(|v| {
            v.get("event").and_then(Value::as_str) == Some("stream")
                && v.get("line")
                    .and_then(Value::as_str)
                    .and_then(|l| serde_json::from_str::<Value>(l).ok())
                    .is_some_and(|inner| {
                        inner.get("type").and_then(Value::as_str) == Some("result")
                    })
        }),
        "streaming continuation mismatch must not emit a normalized result record: {events:?}"
    );
    let refusal = refusal_event(&output.stderr).expect("expected session_lost refusal event");
    assert_eq!(refusal["refusal"]["kind"], "session_lost");
    assert!(refusal["session"].is_null());
}

#[test]
fn host_mode_continue_uses_populated_proj_engine_home_as_child_home() {
    let dir = TempDir::new().unwrap();
    let seen_home_file = dir.path().join("seen-home");
    let fixture = write_executable(
        dir.path(),
        "ag-continue-home",
        &format!(
            "#!/bin/sh\ncat >/dev/null\nprintf '%s' \"$HOME\" > \"{}\"\nprintf '{{\"type\":\"result\",\"conversation_id\":\"ag-old-1\",\"result\":\"continued\"}}\\n'\n",
            seen_home_file.display()
        ),
    );
    let populated_home = dir.path().join("populated-home");
    fs::create_dir_all(&populated_home).unwrap();
    let marker = populated_home.join("marker");
    fs::write(&marker, "prior-state").unwrap();

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

    let output = run_continue(&cfg_path, &cfg, dir.path(), "ag-old-1", |cmd| {
        cmd.env("PROJ_ENGINE_HOME", &populated_home);
    });
    assert!(
        output.status.success(),
        "expected continuation with populated PROJ_ENGINE_HOME to succeed, stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let seen_home = PathBuf::from(fs::read_to_string(&seen_home_file).unwrap());
    assert_eq!(
        seen_home, populated_home,
        "child HOME must be PROJ_ENGINE_HOME rather than a fresh tempdir"
    );
    assert_eq!(
        fs::read_to_string(&marker).unwrap(),
        "prior-state",
        "populated PROJ_ENGINE_HOME state must be preserved"
    );
}

#[test]
fn host_mode_continue_defers_invalid_proj_engine_home_to_path_validation() {
    let dir = TempDir::new().unwrap();
    let real_home = dir.path().join("real-home");
    fs::create_dir_all(&real_home).unwrap();
    let ran_marker = dir.path().join("cli-ran");
    let fixture = write_executable(
        dir.path(),
        "ag-should-not-run",
        &format!(
            "#!/bin/sh\necho ran > \"{}\"\nprintf '{{\"type\":\"result\",\"conversation_id\":\"ag-old-1\",\"result\":\"ok\"}}\\n'\n",
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

    for (label, engine_home, expected_err) in [
        (
            "relative",
            Path::new("relative/engine-home"),
            "must be an absolute path",
        ),
        ("matches-home", real_home.as_path(), "must not match HOME"),
    ] {
        let output = run_continue(&cfg_path, &cfg, dir.path(), "ag-old-1", |cmd| {
            cmd.env("HOME", &real_home)
                .env("PROJ_ENGINE_HOME", engine_home);
        });
        assert!(!output.status.success(), "{label}: expected failure");
        assert!(
            !ran_marker.exists(),
            "{label}: CLI must not run when PROJ_ENGINE_HOME is invalid"
        );
        assert!(
            refusal_event(&output.stderr).is_none(),
            "{label}: invalid PROJ_ENGINE_HOME must not be misclassified as session_lost: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(expected_err),
            "{label}: expected {expected_err:?} in stderr, got: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[cfg(unix)]
#[test]
fn host_mode_continue_does_not_treat_unreadable_proj_engine_home_as_session_lost() {
    let dir = TempDir::new().unwrap();
    let fixture = write_executable(
        dir.path(),
        "ag-continue",
        "#!/bin/sh\ncat >/dev/null\nprintf '{\"type\":\"result\",\"conversation_id\":\"ag-old-1\",\"result\":\"continued\"}\\n'\n",
    );
    let unreadable_home = dir.path().join("unreadable-home");
    fs::create_dir_all(&unreadable_home).unwrap();
    fs::write(unreadable_home.join("marker"), "prior-state").unwrap();
    burrow_test::permissions(&unreadable_home, 0o000);

    // If running as root (e.g., privileged container), read_dir still succeeds.
    if fs::read_dir(&unreadable_home).is_ok() {
        burrow_test::permissions(&unreadable_home, 0o700);
        return;
    }

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

    let output = run_continue(&cfg_path, &cfg, dir.path(), "ag-old-1", |cmd| {
        cmd.env("PROJ_ENGINE_HOME", &unreadable_home);
    });

    burrow_test::permissions(&unreadable_home, 0o700);

    assert!(
        refusal_event(&output.stderr).is_none(),
        "unreadable PROJ_ENGINE_HOME must not be misclassified as session_lost: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
