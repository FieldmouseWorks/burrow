use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use burrow_engine_antigravity::config::{Config, CredentialMode, EFFORTS, ENGINE, ModelSpec};
use conary_core::plugin::{
    Answer, CONTRACT, Context, Execution, Identity, Negotiated, Request, Response, Task,
};
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

fn job_request(cfg: &Config, workdir: &Path) -> Request {
    Request {
        contract: CONTRACT.to_owned(),
        execution: Some(Execution {
            choice: None,
            context: Context::Fresh,
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
            prompt: "do the task".to_owned(),
            model: None,
            effort: None,
        },
    }
}

fn run_plugin_streaming(
    cfg_path: &Path,
    req: &Request,
    envs: &[(&str, &str)],
) -> std::process::Output {
    let mut cmd = Command::new(burrow_test::program!("burrow-engine-antigravity"));
    cmd.arg(cfg_path)
        .env("PROJ_STREAM_EVENTS", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(&serde_json::to_vec(req).unwrap())
        .unwrap();
    drop(child.stdin.take());
    child.wait_with_output().unwrap()
}

#[test]
fn streamed_tool_output_and_result_scrub_configured_credential_file() {
    let dir = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let cred_file = dir.path().join("oauth.json");
    let raw_cred = r#"{"access_token":"ya29.stream-secret-access-1234","refresh_token":"1//stream-secret-refresh-5678","extra_field":"custom-unlisted-secret-9999"}"#;
    fs::write(&cred_file, format!("{raw_cred}\n")).unwrap();

    let tool_raw_cred = serde_json::json!({
        "event": "step_update",
        "step_update": {
            "step_type": "tool",
            "tool_name": "run_command",
            "tool_info": {
                "output": format!("{raw_cred}\n"),
            },
        },
    })
    .to_string();
    let tool_tokens = serde_json::json!({
        "event": "step_update",
        "step_update": {
            "step_type": "tool",
            "tool_name": "run_command",
            "tool_info": {
                "output": "a=ya29.stream-secret-access-1234 r=1//stream-secret-refresh-5678",
            },
        },
    })
    .to_string();
    let result_line = serde_json::json!({
        "event": "result",
        "result": {
            "conversation_id": "conv-scrub-1",
            "status": "SUCCESS",
            "response": "saw ya29.stream-secret-access-1234",
        },
    })
    .to_string();

    let fixture = write_executable(
        dir.path(),
        "ag-stream-leak",
        &format!(
            r#"#!/bin/sh
cat >/dev/null
printf '%s\n' '{{"event":"init","conversation_id":"conv-scrub-1","init":{{"model":"model-a"}}}}'
printf '%s\n' '{tool_raw_cred}'
printf '%s\n' '{tool_tokens}'
printf '%s\n' '{result_line}'
"#
        ),
    );

    let cfg = Config {
        binary: fixture,
        models: sample_models(),
        default_model: Some("test-model".to_owned()),
        credential_mode: CredentialMode::HomeFile,
        credential_file: Some(cred_file),
        deadline_secs: Some(5),
        activity_secs: 60,
        ..Config::default()
    };
    let cfg_path = dir.path().join("cfg.json");
    fs::write(&cfg_path, serde_json::to_vec(&cfg).unwrap()).unwrap();

    let req = job_request(&cfg, dir.path());
    let output = run_plugin_streaming(&cfg_path, &req, &[("HOME", home.path().to_str().unwrap())]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    for secret in [
        raw_cred,
        "ya29.stream-secret-access-1234",
        "1//stream-secret-refresh-5678",
        "custom-unlisted-secret-9999",
    ] {
        assert!(
            !stderr.contains(secret),
            "secret {secret:?} leaked into streamed stderr:\n{stderr}"
        );
        assert!(
            !stdout.contains(secret),
            "secret {secret:?} leaked into stdout response:\n{stdout}"
        );
    }

    let stream_lines: Vec<serde_json::Value> = stderr
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|ev| ev["event"] == "stream")
        .map(|ev| {
            serde_json::from_str::<serde_json::Value>(ev["line"].as_str().unwrap())
                .expect("streamed line must stay valid JSON after scrubbing")
        })
        .collect();
    assert_eq!(stream_lines.len(), 4, "stream_lines: {stream_lines:?}");
    assert_eq!(
        stream_lines[1]["step_update"]["tool_info"]["output"],
        "[redacted]\n"
    );
    assert_eq!(
        stream_lines[2]["step_update"]["tool_info"]["output"],
        "a=[redacted] r=[redacted]"
    );
    assert_eq!(stream_lines[3]["type"], "result");
    assert_eq!(stream_lines[3]["result"], "saw [redacted]");

    let response: Response = serde_json::from_str(&stdout).unwrap();
    match response.answer {
        Answer::Job { summary, .. } => assert_eq!(summary, "saw [redacted]"),
        other => panic!("expected Answer::Job, got {other:?}"),
    }
}

#[test]
fn streamed_split_chunks_and_error_result_scrub_env_credential() {
    let dir = TempDir::new().unwrap();
    let secret = "ag-split-secret-token-424242";

    // 1. Secret split across two agent_response text_delta chunks reassembled in normalized result.
    let split_fixture = write_executable(
        dir.path(),
        "ag-split-chunks",
        r#"#!/bin/sh
cat >/dev/null
printf '%s\n' '{"event":"init","conversation_id":"conv-split-1"}'
printf '%s\n' '{"event":"step_update","step_update":{"step_type":"agent_response","text_delta":"tok=ag-split-secret-"}}'
printf '%s\n' '{"event":"step_update","step_update":{"step_type":"agent_response","text_delta":"token-424242"}}'
printf '%s\n' '{"event":"result","result":{"conversation_id":"conv-split-1","status":"SUCCESS","response":""}}'
"#,
    );
    let cfg = Config {
        binary: split_fixture,
        models: sample_models(),
        default_model: Some("test-model".to_owned()),
        credential_mode: CredentialMode::Env,
        credential_env: Some("BURROW_AG_SECRET_IN".to_owned()),
        target_credential_env: Some("AG_TARGET_CRED".to_owned()),
        deadline_secs: Some(5),
        activity_secs: 60,
        ..Config::default()
    };
    let cfg_path = dir.path().join("split-cfg.json");
    fs::write(&cfg_path, serde_json::to_vec(&cfg).unwrap()).unwrap();
    let req = job_request(&cfg, dir.path());

    let output = run_plugin_streaming(&cfg_path, &req, &[("BURROW_AG_SECRET_IN", secret)]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains(secret),
        "secret leaked in stderr:\n{stderr}"
    );
    assert!(
        !stdout.contains(secret),
        "secret leaked in stdout:\n{stdout}"
    );

    let normalized_result = stderr
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|ev| ev["event"] == "stream")
        .filter_map(|ev| serde_json::from_str::<serde_json::Value>(ev["line"].as_str()?).ok())
        .find(|v| v["type"] == "result")
        .expect("missing normalized result stream event");
    assert_eq!(normalized_result["result"], "tok=[redacted]");

    let response: Response = serde_json::from_str(&stdout).unwrap();
    match response.answer {
        Answer::Job { summary, .. } => assert_eq!(summary, "tok=[redacted]"),
        other => panic!("expected Answer::Job, got {other:?}"),
    }

    // 2. Secret echoed in an error result envelope.
    let err_fixture = write_executable(
        dir.path(),
        "ag-err-secret",
        &format!(
            r#"#!/bin/sh
cat >/dev/null
printf '%s\n' '{{"event":"result","result":{{"conversation_id":"conv-err-sec","status":"ERROR","response":"","error":"failed with {secret}"}}}}'
exit 1
"#
        ),
    );
    let mut err_cfg = cfg;
    err_cfg.binary = err_fixture;
    let err_cfg_path = dir.path().join("err-cfg.json");
    fs::write(&err_cfg_path, serde_json::to_vec(&err_cfg).unwrap()).unwrap();

    let err_out = run_plugin_streaming(&err_cfg_path, &req, &[("BURROW_AG_SECRET_IN", secret)]);
    assert!(!err_out.status.success());
    let err_stderr = String::from_utf8_lossy(&err_out.stderr);
    assert!(
        !err_stderr.contains(secret),
        "secret leaked in error stderr:\n{err_stderr}"
    );
    assert!(err_stderr.contains("[redacted]"), "{err_stderr}");
}

#[test]
fn timed_out_stream_preserves_session_and_scrubs_credential_file() {
    let dir = TempDir::new().unwrap();
    let cred_file = dir.path().join("cred.txt");
    let secret = "ag-timeout-secret-value-9988";
    fs::write(&cred_file, format!("{secret}\n")).unwrap();

    let tool_line = serde_json::json!({
        "event": "step_update",
        "step_update": {
            "step_type": "tool",
            "tool_name": "run_command",
            "tool_info": {
                "output": format!("leaked {secret}"),
            },
        },
    })
    .to_string();

    let fixture = write_executable(
        dir.path(),
        "ag-timeout-leak",
        &format!(
            r#"#!/bin/sh
printf '%s\n' '{{"event":"init","conversation_id":"conv-timeout-scrub"}}'
printf '%s\n' '{tool_line}'
sleep 10
"#
        ),
    );

    let cfg = Config {
        binary: fixture,
        models: sample_models(),
        default_model: Some("test-model".to_owned()),
        credential_mode: CredentialMode::File,
        credential_file: Some(cred_file),
        target_credential_env: Some("AG_CRED_PATH".to_owned()),
        deadline_secs: Some(1),
        activity_secs: 60,
        ..Config::default()
    };
    let cfg_path = dir.path().join("timeout-cfg.json");
    fs::write(&cfg_path, serde_json::to_vec(&cfg).unwrap()).unwrap();

    let req = job_request(&cfg, dir.path());
    let output = run_plugin_streaming(&cfg_path, &req, &[]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains(secret),
        "secret leaked into timed-out stderr:\n{stderr}"
    );
    let events: Vec<serde_json::Value> = stderr
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    assert!(
        events
            .iter()
            .any(|e| e["event"] == "session" && e["session_id"] == "conv-timeout-scrub"),
        "missing session event:\n{stderr}"
    );
    assert!(
        events.iter().any(|e| {
            e["event"] == "stream"
                && e["line"]
                    .as_str()
                    .is_some_and(|l| l.contains("leaked [redacted]"))
        }),
        "missing redacted tool output stream line:\n{stderr}"
    );
    assert!(
        events
            .iter()
            .any(|e| e["event"] == "error" && e["session"] == "conv-timeout-scrub"),
        "missing preserved session on timeout error:\n{stderr}"
    );
}
