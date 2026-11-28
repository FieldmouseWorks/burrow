use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use burrow_engine_antigravity::config::{Config, EFFORTS, ENGINE, ModelSpec};
use conary_core::plugin::{CONTRACT, Context, Execution, Identity, Negotiated, Request, Task};
use tempfile::TempDir;

fn write_executable(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    burrow_test::executable(&path, body);
    path
}

fn sample_models() -> Vec<ModelSpec> {
    vec![
        ModelSpec {
            id: "test-model".to_owned(),
            efforts: EFFORTS.iter().map(|e| (*e).to_owned()).collect(),
            context_tokens: Some(200_000),
            max_output_tokens: Some(16_384),
        },
        ModelSpec {
            id: "alt-model".to_owned(),
            efforts: vec!["low".to_owned(), "high".to_owned(), "max".to_owned()],
            context_tokens: None,
            max_output_tokens: None,
        },
    ]
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

#[test]
fn streams_events_and_preserves_session_when_invocation_times_out() {
    let dir = TempDir::new().unwrap();
    let init_line =
        r#"{"event":"init","init":{"session_id":"ag-timeout-sess-1","model":"model-a"}}"#;
    let bin = write_executable(
        dir.path(),
        "fake-antigravity",
        &format!("#!/bin/sh\nprintf '%s\\n' '{init_line}'\nsleep 10\n"),
    );
    let cfg = Config {
        binary: bin,
        models: sample_models(),
        default_model: Some("test-model".to_owned()),
        deadline_secs: Some(1),
        activity_secs: 60,
        ..Config::default()
    };
    let cfg_path = dir.path().join("cfg.json");
    fs::write(&cfg_path, serde_json::to_vec(&cfg).unwrap()).unwrap();

    let req = job_request(&cfg, dir.path());
    let mut command = Command::new(burrow_test::program!("burrow-engine-antigravity"));
    command
        .arg(&cfg_path)
        .env("PROJ_STREAM_EVENTS", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(&serde_json::to_vec(&req).unwrap())
        .unwrap();
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());

    let stderr = String::from_utf8_lossy(&output.stderr);
    let events: Vec<serde_json::Value> = stderr
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    assert!(
        events
            .iter()
            .any(|e| e["event"] == "stream" && e["line"] == init_line),
        "expected streamed init line in stderr: {stderr}"
    );
    assert!(
        events
            .iter()
            .any(|e| e["event"] == "session" && e["session_id"] == "ag-timeout-sess-1"),
        "expected session event in stderr: {stderr}"
    );
    assert!(
        events
            .iter()
            .any(|e| e["event"] == "error" && e["session"] == "ag-timeout-sess-1"),
        "expected session preserved on error diagnostic: {stderr}"
    );
}

#[test]
fn streams_normalized_result_event_before_cli_exits() {
    let dir = TempDir::new().unwrap();
    let init_line =
        r#"{"event":"init","conversation_id":"conv-hang-1","init":{"model":"model-a"}}"#;
    let result_line = r#"{"event":"result","result":{"conversation_id":"conv-hang-1","status":"SUCCESS","response":"Final answer\n","num_turns":1,"usage":{"input_tokens":120,"output_tokens":15,"cache_read_tokens":40,"total_tokens":135,"cost_usd":0.03}}}"#;
    let bin = write_executable(
        dir.path(),
        "fake-antigravity-hang-after-result",
        &format!(
            "#!/bin/sh\nprintf '%s\\n' '{init_line}'\nprintf '%s\\n' '{result_line}'\nsleep 5\n"
        ),
    );
    let cfg = Config {
        binary: bin,
        models: sample_models(),
        default_model: Some("test-model".to_owned()),
        deadline_secs: Some(3),
        activity_secs: 60,
        ..Config::default()
    };
    let cfg_path = dir.path().join("cfg.json");
    fs::write(&cfg_path, serde_json::to_vec(&cfg).unwrap()).unwrap();

    let req = job_request(&cfg, dir.path());
    let mut child = Command::new(burrow_test::program!("burrow-engine-antigravity"))
        .arg(&cfg_path)
        .env("PROJ_STREAM_EVENTS", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(&serde_json::to_vec(&req).unwrap())
        .unwrap();
    drop(child.stdin.take());

    let stderr = child.stderr.take().unwrap();
    let mut seen_stream_lines: Vec<serde_json::Value> = Vec::new();
    let mut found_result: Option<serde_json::Value> = None;
    for line in BufReader::new(stderr).lines().map_while(Result::ok) {
        let Ok(ev) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if ev["event"] == "stream"
            && let Some(raw) = ev["line"].as_str()
            && let Ok(parsed) = serde_json::from_str::<serde_json::Value>(raw)
        {
            if parsed.get("type").and_then(|v| v.as_str()) == Some("result") {
                found_result = Some(parsed.clone());
                let _ = child.kill();
                break;
            }
            seen_stream_lines.push(parsed);
        }
    }
    let _ = child.wait();

    let result = found_result.unwrap_or_else(|| {
        panic!(
            "expected normalized top-level type=result stream event before CLI exited; saw: {seen_stream_lines:?}"
        )
    });
    assert_eq!(result["type"], "result");
    assert_eq!(result["subtype"], "success");
    assert_eq!(result["is_error"], false);
    assert_eq!(result["result"], "Final answer\n");
    assert_eq!(result["session_id"], "conv-hang-1");
    assert_eq!(result["num_turns"], 1);
    assert_eq!(result["usage"]["input_tokens"], 120);
    assert_eq!(result["usage"]["output_tokens"], 15);
    assert_eq!(result["usage"]["cache_read_input_tokens"], 40);
    assert_eq!(result["usage"]["cache_creation_input_tokens"], 0);
    assert_eq!(result["total_cost_usd"], 0.03);
}

#[test]
fn streams_normalized_error_result_and_emits_session_before_result_line() {
    let dir = TempDir::new().unwrap();
    let err_line = r#"{"event":"result","result":{"conversation_id":"conv-err-only","status":"ERROR","response":"","error":"internal failure"}}"#;
    let bin = write_executable(
        dir.path(),
        "fake-antigravity-err-result",
        &format!("#!/bin/sh\nprintf '%s\\n' '{err_line}'\nexit 1\n"),
    );
    let cfg = Config {
        binary: bin,
        models: sample_models(),
        default_model: Some("test-model".to_owned()),
        deadline_secs: Some(5),
        activity_secs: 60,
        ..Config::default()
    };
    let cfg_path = dir.path().join("cfg.json");
    fs::write(&cfg_path, serde_json::to_vec(&cfg).unwrap()).unwrap();

    let req = job_request(&cfg, dir.path());
    let mut child = Command::new(burrow_test::program!("burrow-engine-antigravity"))
        .arg(&cfg_path)
        .env("PROJ_STREAM_EVENTS", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(&serde_json::to_vec(&req).unwrap())
        .unwrap();
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());

    let stderr = String::from_utf8_lossy(&output.stderr);
    let events: Vec<serde_json::Value> = stderr
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let session_idx = events
        .iter()
        .position(|e| e["event"] == "session" && e["session_id"] == "conv-err-only")
        .unwrap_or_else(|| panic!("missing session event in stderr: {stderr}"));
    let stream_idx = events
        .iter()
        .position(|e| e["event"] == "stream")
        .unwrap_or_else(|| panic!("missing stream event in stderr: {stderr}"));
    assert!(
        session_idx < stream_idx,
        "session event must precede normalized result stream line: {stderr}"
    );
    let parsed: serde_json::Value =
        serde_json::from_str(events[stream_idx]["line"].as_str().unwrap()).unwrap();
    assert_eq!(parsed["type"], "result");
    assert_eq!(parsed["subtype"], "error");
    assert_eq!(parsed["is_error"], true);
    assert_eq!(parsed["result"], "internal failure");
    assert_eq!(parsed["session_id"], "conv-err-only");
}
