//! Local fake HTTP API endpoint tests for `claude-api` mode.

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::Duration;

use burrow_engine_anthropic::plugin::{
    DiagnosticEvent, EXIT_FAILURE, EXIT_REFUSED_AUTH, EXIT_REFUSED_USAGE, RefusalKind,
};
use conary_core::plugin::{Answer, Choice, Context, Next, Plugin, Task};
use serde_json::{Value, json};

use crate::common::{
    TEST_MODEL_A, TEST_MODEL_B, default_limits, invoke_raw, make_v5_job_request, make_wrapper,
    parse_diagnostics, test_models, write_config,
};

/// Captured request observed by the local fake Anthropic HTTP API endpoint.
#[derive(Clone, Debug)]
struct ApiHit {
    api_key: Option<String>,
    body: Value,
}

/// Local loopback HTTP server standing in for the Anthropic Messages API.
struct FakeApiServer {
    url: String,
    hits: Arc<Mutex<Vec<ApiHit>>>,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl FakeApiServer {
    fn start(expected_key: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();

        let hits = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let hits_bg = hits.clone();
        let stop_bg = stop.clone();
        let expected_key = expected_key.to_owned();

        let handle = thread::spawn(move || {
            while !stop_bg.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                        let (headers, body_bytes) = read_http_request(&mut stream);
                        let key = headers.get("x-api-key").cloned();
                        let body: Value =
                            serde_json::from_slice(&body_bytes).unwrap_or(Value::Null);
                        hits_bg.lock().unwrap().push(ApiHit {
                            api_key: key.clone(),
                            body: body.clone(),
                        });
                        let (status, resp_json) =
                            route_fake_api(&expected_key, key.as_deref(), &body);
                        let resp_body = resp_json.to_string();
                        let http = format!(
                            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp_body}",
                            resp_body.len()
                        );
                        let _ = stream.write_all(http.as_bytes());
                        let _ = stream.flush();
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            url: format!("http://{addr}"),
            hits,
            stop,
            handle: Some(handle),
        }
    }

    fn hits(&self) -> Vec<ApiHit> {
        self.hits.lock().unwrap().clone()
    }
}

impl Drop for FakeApiServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn read_http_request(stream: &mut impl Read) -> (BTreeMap<String, String>, Vec<u8>) {
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    let _ = reader.read_line(&mut request_line);
    let mut headers = BTreeMap::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some((k, v)) = trimmed.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_owned());
        }
    }
    let len: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; len];
    if len > 0 {
        let _ = reader.read_exact(&mut body);
    }
    (headers, body)
}

fn route_fake_api(
    expected_key: &str,
    given_key: Option<&str>,
    body: &Value,
) -> (&'static str, Value) {
    if given_key != Some(expected_key) {
        return (
            "401 Unauthorized",
            json!({
                "error": "authentication_failed",
                "message": "Invalid API key · Fix external API key"
            }),
        );
    }
    let prompt = body["prompt"].as_str().unwrap_or_default();
    let model = body["model"].as_str().unwrap_or_default();
    let effort = body["effort"].as_str().unwrap_or_default();
    let resume = body["resume"].as_str().unwrap_or_default();

    if prompt == "api-credit-limit" {
        return (
            "429 Too Many Requests",
            json!({
                "error": "usage_limit_reached",
                "message": "Your credit balance is too low to access the Anthropic API."
            }),
        );
    }
    if prompt == "api-budget-exceeded" {
        return (
            "200 OK",
            json!({
                "subtype": "error_max_budget_usd",
                "is_error": true,
                "session_id": "api-sess-budget",
                "total_cost_usd": 1.25
            }),
        );
    }
    if prompt == "api-server-error" {
        return (
            "500 Internal Server Error",
            json!({
                "error": "server_error",
                "message": "Internal server error"
            }),
        );
    }
    let session = if resume.is_empty() {
        "api-sess-01"
    } else {
        resume
    };
    (
        "200 OK",
        json!({
            "subtype": "success",
            "is_error": false,
            "session_id": session,
            "answer": format!("api-ok session={session} model={model} effort={effort}"),
            "total_cost_usd": 0.032,
            "usage": {
                "input_tokens": 210,
                "output_tokens": 64,
                "cache_read_input_tokens": 0,
                "cache_creation_input_tokens": 0
            }
        }),
    )
}

/// Fake `claude` CLI for `claude-api` mode: asserts no API/login env vars were
/// leaked, reads the API key strictly from `CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR`,
/// posts to `$ANTHROPIC_BASE_URL/v1/messages`, and emits Claude stream-JSONL.
fn install_fake_claude_api_cli(path: &Path) {
    let script = r#"#!/bin/sh
set -eu
# Credentials must arrive ONLY via CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR.
if [ -n "${ANTHROPIC_API_KEY:-}" ] || [ -n "${ANTHROPIC_AUTH_TOKEN:-}" ] || [ -n "${CLAUDE_CODE_OAUTH_TOKEN:-}" ]; then
  echo "leaked credential in child environment" >&2
  exit 91
fi
if [ -n "${GH_TOKEN:-}" ] || [ -n "${GITHUB_TOKEN:-}" ] || [ -n "${GH_TOKEN_SOCKET:-}" ]; then
  echo "leaked github token in child environment" >&2
  exit 92
fi
if [ -z "${CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR:-}" ]; then
  echo "missing CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR" >&2
  exit 93
fi
api_key="$(tr -d '\r\n' < "/dev/fd/$CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR")"
prompt="$(cat)"
model=""
effort=""
resume=""
budget=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --model) shift; model="$1" ;;
    --effort) shift; effort="$1" ;;
    --resume) shift; resume="$1" ;;
    --max-budget-usd) shift; budget="$1" ;;
  esac
  shift
done

payload="$(jq -nc --arg prompt "$prompt" --arg model "$model" --arg effort "$effort" --arg resume "$resume" --arg budget "$budget" \
  '{prompt: $prompt, model: $model, effort: $effort, resume: $resume, budget: $budget}')"

tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT
code="$(curl -sS -o "$tmp" -w '%{http_code}' -H "x-api-key: $api_key" -H "content-type: application/json" --data "$payload" "$ANTHROPIC_BASE_URL/v1/messages")"

case "$code" in
  200)
    sid="$(jq -r '.session_id' "$tmp")"
    jq -nc --arg sid "$sid" '{type: "system", subtype: "init", session_id: $sid, apiKeySource: "ANTHROPIC_API_KEY"}'
    subtype="$(jq -r '.subtype' "$tmp")"
    if [ "$subtype" = "error_max_budget_usd" ]; then
      jq -c '{type: "result", subtype: .subtype, is_error: true, session_id: .session_id, total_cost_usd: .total_cost_usd}' "$tmp"
    else
      jq -c '{type: "result", subtype: "success", is_error: false, session_id: .session_id, result: .answer, total_cost_usd: .total_cost_usd, usage: .usage}' "$tmp"
    fi
    ;;
  401)
    printf '%s\n' '{"type":"system","subtype":"init","session_id":"api-auth","apiKeySource":"ANTHROPIC_API_KEY"}'
    msg="$(jq -r '.message' "$tmp")"
    jq -nc --arg msg "$msg" '{type: "assistant", error: "authentication_failed", session_id: "api-auth", message: {content: [{type: "text", text: $msg}]}}'
    jq -nc --arg msg "$msg" '{type: "result", subtype: "success", is_error: true, session_id: "api-auth", result: $msg, total_cost_usd: 0}'
    ;;
  429)
    printf '%s\n' '{"type":"system","subtype":"init","session_id":"api-limit","apiKeySource":"ANTHROPIC_API_KEY"}'
    msg="$(jq -r '.message' "$tmp")"
    jq -nc --arg msg "$msg" '{type: "result", subtype: "success", is_error: true, api_error: "usage_limit_reached", session_id: "api-limit", result: $msg, total_cost_usd: 0}'
    ;;
  *)
    printf '%s\n' '{"type":"system","subtype":"init","session_id":"api-err","apiKeySource":"ANTHROPIC_API_KEY"}'
    msg="$(jq -r '.message // "server error"' "$tmp")"
    jq -nc --arg msg "$msg" '{type: "result", subtype: "error_during_execution", is_error: true, session_id: "api-err", result: $msg}'
    exit 1
    ;;
esac
"#;
    burrow_test::executable(path, script);
}

#[test]
fn claude_api_mode_against_local_fake_api_endpoint() {
    let dir = tempfile::tempdir().unwrap();
    let workdir = dir.path().join("repo");
    fs::create_dir_all(&workdir).unwrap();

    let api_key = "stand-in-fixture-key-value";
    let server = FakeApiServer::start(api_key);
    let fake_cli = dir.path().join("fake-claude-api");
    install_fake_claude_api_cli(&fake_cli);

    let cfg_path = write_config(
        dir.path(),
        "claude-api.json",
        &json!({
            "mode": "claude-api",
            "models": test_models(),
            "defaultModel": TEST_MODEL_A,
            "defaultEffort": "low",
            "program": fake_cli,
            "apiBaseUrl": server.url,
            "maxBudgetUsd": 5_000_000,
        }),
    );
    let offers = burrow_engine_anthropic::plugin::PluginConfig::from_file(&cfg_path)
        .unwrap()
        .offers();

    // 1. Fresh job via Conary runner: API key delivered via credentialEnv -> FD,
    // never env var, and model/effort/budget reach the fake HTTP API endpoint.
    let wrapper = make_wrapper(
        dir.path(),
        "plugin-claude-api",
        &cfg_path,
        &[
            ("ANTHROPIC_API_KEY", api_key),
            ("CLAUDE_CODE_OAUTH_TOKEN", "must-never-reach-api-mode"),
            ("GH_TOKEN", "ghp_must_never_be_forwarded"),
            ("PROJ_CAPABILITIES", r#"["workspace","network"]"#),
        ],
    );
    let plugin = Plugin::describe(&wrapper, default_limits()).unwrap();
    let resp = plugin
        .call_with(
            Task::Job {
                repo: "owner/demo".into(),
                workdir: workdir.to_string_lossy().into_owned(),
                prompt: "fresh api work".into(),
                model: Some(TEST_MODEL_B.into()),
                effort: Some("high".into()),
            },
            Some(Choice {
                engine: "claude-api".into(),
                model: Some(TEST_MODEL_B.into()),
                effort: Some("high".into()),
            }),
            Context::Fresh,
            None,
        )
        .expect("fresh claude-api job succeeds");
    assert_eq!(resp.by.engine, "claude-api");
    assert_eq!(resp.by.session.as_deref(), Some("api-sess-01"));
    match resp.answer {
        Answer::Job { summary, next } => {
            assert_eq!(
                summary,
                format!("api-ok session=api-sess-01 model={TEST_MODEL_B} effort=high")
            );
            assert_eq!(next, Next::Done);
        }
        other => panic!("unexpected answer: {other:?}"),
    }

    // 2. Continuation resumes the returned session through the fake API endpoint.
    let cont = plugin
        .call_with(
            Task::Job {
                repo: "owner/demo".into(),
                workdir: workdir.to_string_lossy().into_owned(),
                prompt: "follow-up api work".into(),
                model: None,
                effort: None,
            },
            None,
            Context::Continue {
                session: "api-sess-01".into(),
            },
            None,
        )
        .expect("continuation claude-api job succeeds");
    assert_eq!(cont.by.session.as_deref(), Some("api-sess-01"));
    let hits = server.hits();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].api_key.as_deref(), Some(api_key));
    assert_eq!(hits[0].body["model"], TEST_MODEL_B);
    assert_eq!(hits[0].body["effort"], "high");
    assert_eq!(hits[0].body["budget"], "5.000000");
    assert_eq!(hits[1].body["resume"], "api-sess-01");
    assert_eq!(hits[1].body["model"], TEST_MODEL_A);
    assert_eq!(hits[1].body["effort"], "low");

    // 3. Invalid API key (401) -> EXIT_REFUSED_AUTH + ApiKeyRejected.
    let req = make_v5_job_request(
        "claude-api",
        offers.clone(),
        &workdir,
        "bad key",
        None,
        Context::Fresh,
    );
    let out = invoke_raw(&cfg_path, &req, &[("ANTHROPIC_API_KEY", "wrong-key")]);
    assert_eq!(out.status.code(), Some(i32::from(EXIT_REFUSED_AUTH)));
    assert!(out.stdout.is_empty());
    assert!(matches!(
        parse_diagnostics(&out.stderr).as_slice(),
        [DiagnosticEvent::Refusal { refusal, .. }] if refusal.kind == RefusalKind::ApiKeyRejected
    ));

    // 4. Missing API key -> EXIT_REFUSED_AUTH before hitting the endpoint.
    let hits_before = server.hits().len();
    let out = invoke_raw(&cfg_path, &req, &[]);
    assert_eq!(out.status.code(), Some(i32::from(EXIT_REFUSED_AUTH)));
    assert_eq!(server.hits().len(), hits_before);

    // 5. Credit / usage limit (429) -> EXIT_REFUSED_USAGE + CreditExhausted.
    let req = make_v5_job_request(
        "claude-api",
        offers.clone(),
        &workdir,
        "api-credit-limit",
        None,
        Context::Fresh,
    );
    let out = invoke_raw(&cfg_path, &req, &[("ANTHROPIC_API_KEY", api_key)]);
    assert_eq!(out.status.code(), Some(i32::from(EXIT_REFUSED_USAGE)));
    assert!(out.stdout.is_empty());
    assert!(matches!(
        parse_diagnostics(&out.stderr).as_slice(),
        [DiagnosticEvent::Refusal { refusal, .. }] if refusal.kind == RefusalKind::CreditExhausted
    ));

    // 6. Budget exhausted (`error_max_budget_usd`) -> EXIT_REFUSED_USAGE + BudgetExhausted.
    let req = make_v5_job_request(
        "claude-api",
        offers.clone(),
        &workdir,
        "api-budget-exceeded",
        None,
        Context::Fresh,
    );
    let out = invoke_raw(&cfg_path, &req, &[("ANTHROPIC_API_KEY", api_key)]);
    assert_eq!(out.status.code(), Some(i32::from(EXIT_REFUSED_USAGE)));
    assert!(out.stdout.is_empty());
    assert!(matches!(
        parse_diagnostics(&out.stderr).as_slice(),
        [DiagnosticEvent::Refusal { refusal, .. }] if refusal.kind == RefusalKind::BudgetExhausted
    ));

    // 7. Server 500 error -> EXIT_FAILURE.
    let req = make_v5_job_request(
        "claude-api",
        offers.clone(),
        &workdir,
        "api-server-error",
        None,
        Context::Fresh,
    );
    let out = invoke_raw(&cfg_path, &req, &[("ANTHROPIC_API_KEY", api_key)]);
    assert_eq!(out.status.code(), Some(i32::from(EXIT_FAILURE)));
    assert!(out.stdout.is_empty());
    assert!(matches!(
        parse_diagnostics(&out.stderr).as_slice(),
        [DiagnosticEvent::Error { .. }]
    ));

    // 8. Credit meter admission refusal (credit off) -> EXIT_REFUSED_USAGE before calling API.
    let credit_file = dir.path().join("overlay.credit.json");
    let jobs_dir = dir.path().join("jobs");
    fs::create_dir_all(&jobs_dir).unwrap();
    fs::write(&credit_file, r#"{"credit":null}"#).unwrap();
    let metered_cfg = write_config(
        dir.path(),
        "claude-api-metered.json",
        &json!({
            "mode": "claude-api",
            "models": test_models(),
            "defaultModel": TEST_MODEL_A,
            "program": fake_cli,
            "apiBaseUrl": server.url,
            "credit": {
                "creditFile": credit_file,
                "jobsDir": jobs_dir,
            }
        }),
    );
    let hits_before = server.hits().len();
    let req = make_v5_job_request(
        "claude-api",
        offers,
        &workdir,
        "fresh metered",
        None,
        Context::Fresh,
    );
    let out = invoke_raw(&metered_cfg, &req, &[("ANTHROPIC_API_KEY", api_key)]);
    assert_eq!(out.status.code(), Some(i32::from(EXIT_REFUSED_USAGE)));
    assert_eq!(server.hits().len(), hits_before);
    assert!(matches!(
        parse_diagnostics(&out.stderr).as_slice(),
        [DiagnosticEvent::Refusal { refusal, .. }] if refusal.kind == RefusalKind::CreditExhausted
    ));
}
