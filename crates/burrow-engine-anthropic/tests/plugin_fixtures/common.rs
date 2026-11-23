//! Shared helpers and fake `claude` CLI fixture for plugin integration tests.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use burrow_engine_anthropic::plugin::{CONFIG_ENV, DiagnosticEvent};
use conary_core::plugin::{
    CONTRACT, Choice, Context, Execution, Identity, Limits, Model, Negotiated, Offers, Request,
    Task,
};
use serde_json::Value;

pub const TEST_MODEL_A: &str = "test-model-alpha";
pub const TEST_MODEL_B: &str = "test-model-beta";

pub fn plugin_bin() -> PathBuf {
    PathBuf::from(burrow_test::program!("burrow-engine-anthropic"))
}

pub fn default_limits() -> Limits {
    Limits {
        deadline: Duration::from_secs(30),
        response_bytes: 1 << 20,
    }
}

pub fn test_models() -> Vec<Model> {
    vec![
        Model {
            id: TEST_MODEL_A.to_owned(),
            efforts: vec!["low".into(), "medium".into(), "high".into()],
            context_tokens: Some(200_000),
            max_output_tokens: Some(16_384),
        },
        Model {
            id: TEST_MODEL_B.to_owned(),
            efforts: vec!["low".into(), "high".into()],
            context_tokens: Some(100_000),
            max_output_tokens: Some(8_192),
        },
    ]
}

pub fn write_config(dir: &Path, name: &str, cfg: &Value) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, serde_json::to_string_pretty(cfg).unwrap()).unwrap();
    path
}

/// Build a wrapper executable with no arguments (as Conary's runner invokes
/// plugins) pinning `BURROW_ANTHROPIC_CONFIG` and test environment variables.
pub fn make_wrapper(dir: &Path, name: &str, config: &Path, extra_env: &[(&str, &str)]) -> PathBuf {
    let wrapper = dir.join(name);
    let mut script = String::from("#!/bin/sh\n");
    script.push_str(&format!(
        "export {CONFIG_ENV}={}\n",
        shell_quote(&config.to_string_lossy())
    ));
    for (k, v) in extra_env {
        script.push_str(&format!("export {k}={}\n", shell_quote(v)));
    }
    script.push_str(&format!(
        "exec {}\n",
        shell_quote(&plugin_bin().to_string_lossy())
    ));
    burrow_test::executable(&wrapper, script);
    wrapper
}

pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Invoke the plugin binary directly with a JSON request on stdin and capture
/// exit code, stdout, and stderr.
pub fn invoke_raw(config: &Path, request: &Request, env_vars: &[(&str, &str)]) -> Output {
    let mut cmd = burrow_test::command(plugin_bin());
    cmd.env(CONFIG_ENV, config);
    for key in [
        "CLAUDE_CODE_OAUTH_TOKEN",
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "GH_TOKEN",
        "GITHUB_TOKEN",
        "GH_TOKEN_SOCKET",
        "PROJ_CAPABILITIES",
        "PROJ_MCP_SERVERS",
    ] {
        cmd.env_remove(key);
    }
    for (k, v) in env_vars {
        cmd.env(k, v);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let payload = serde_json::to_vec(request).unwrap();
    {
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(&payload).unwrap();
        stdin.write_all(b"\n").unwrap();
    }
    child.wait_with_output().unwrap()
}

pub fn parse_diagnostics(stderr: &[u8]) -> Vec<DiagnosticEvent> {
    String::from_utf8_lossy(stderr)
        .lines()
        .filter_map(|line| serde_json::from_str::<DiagnosticEvent>(line).ok())
        .collect()
}

pub fn make_v5_job_request(
    engine: &str,
    offers: Offers,
    workdir: &Path,
    prompt: &str,
    choice: Option<Choice>,
    context: Context,
) -> Request {
    let model = choice.as_ref().and_then(|c| c.model.clone());
    let effort = choice.as_ref().and_then(|c| c.effort.clone());
    Request {
        contract: CONTRACT.to_owned(),
        execution: Some(Execution { choice, context }),
        negotiated: Some(Negotiated {
            by: Identity {
                engine: engine.to_owned(),
                session: None,
            },
            offers,
        }),
        task: Task::Job {
            repo: "owner/demo".to_owned(),
            workdir: workdir.to_string_lossy().into_owned(),
            prompt: prompt.to_owned(),
            model,
            effort,
        },
    }
}

pub fn wait_until_gone(pid: i32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !Path::new(&format!("/proc/{pid}")).exists() {
            return true;
        }
        thread::sleep(Duration::from_millis(15));
    }
    !Path::new(&format!("/proc/{pid}")).exists()
}

/// Fake `claude` CLI for subscription mode: records argv, stdin, and env into
/// `record_dir`, then replays a recorded stream-JSONL shape selected by prompt.
pub fn install_fake_claude_cli(path: &Path, record_dir: &Path) {
    let rec = record_dir.to_string_lossy();
    let script = format!(
        r#"#!/bin/sh
set -eu
rec={rec}
prompt="$(cat)"
printf '%s' "$prompt" > "$rec/prompt"
: > "$rec/args"
model=""
effort=""
resume=""
while [ "$#" -gt 0 ]; do
  printf '%s\n' "$1" >> "$rec/args"
  case "$1" in
    --model) shift; model="$1"; printf '%s\n' "$1" >> "$rec/args" ;;
    --effort) shift; effort="$1"; printf '%s\n' "$1" >> "$rec/args" ;;
    --resume) shift; resume="$1"; printf '%s\n' "$1" >> "$rec/args" ;;
  esac
  shift
done
env | sort > "$rec/env"

case "$prompt" in
  "fresh:"*)
    printf '%s\n' '{{"type":"system","subtype":"init","session_id":"sess-fresh-01"}}'
    printf '%s\n' '{{"type":"assistant","session_id":"sess-fresh-01","message":{{"id":"msg_1","role":"assistant","content":[{{"type":"text","text":"working"}}],"usage":{{"input_tokens":50,"output_tokens":10,"cache_read_input_tokens":5,"cache_creation_input_tokens":2}}}}}}'
    printf '{{"type":"result","subtype":"success","is_error":false,"session_id":"sess-fresh-01","result":"answered model=%s effort=%s","total_cost_usd":0.015,"usage":{{"input_tokens":120,"output_tokens":45,"cache_read_input_tokens":10,"cache_creation_input_tokens":4}}}}\n' "$model" "$effort"
    ;;
  "continue:"*)
    printf '{{"type":"system","subtype":"init","session_id":"%s"}}\n' "$resume"
    printf '{{"type":"result","subtype":"success","is_error":false,"session_id":"%s","result":"continued %s model=%s effort=%s","total_cost_usd":0.02,"usage":{{"input_tokens":80,"output_tokens":30,"cache_read_input_tokens":15,"cache_creation_input_tokens":0}}}}\n' "$resume" "$resume" "$model" "$effort"
    ;;
  "usage-refusal")
    printf '%s\n' '{{"type":"rate_limit_event","rate_limit_info":{{"status":"allowed","resetsAt":1792836000,"rateLimitType":"five_hour"}}}}'
    printf '%s\n' '{{"type":"assistant","session_id":"sess-limit","error":"rate_limit","is_api_error_message":true,"api_error_status":429,"api_error":"usage_limit_reached","api_error_params":{{"rate_limit_info":{{"status":"rejected","resetsAt":1793822400,"rateLimitType":"seven_day","overageStatus":"rejected"}}}},"message":{{"role":"assistant","content":[{{"type":"text","text":"You'\''ve hit your monthly spend limit · raise it at https://claude.ai/settings/usage · your weekly limit resets Nov 4, 12pm (America/Los_Angeles)"}}]}}}}'
    printf '%s\n' '{{"type":"result","subtype":"success","is_error":true,"num_turns":1,"api_error_status":429,"api_error":"usage_limit_reached","result":"You'\''ve hit your monthly spend limit · raise it at https://claude.ai/settings/usage · your weekly limit resets Nov 4, 12pm (America/Los_Angeles)","session_id":"sess-limit"}}'
    ;;
  "login-refusal")
    printf '%s\n' '{{"type":"assistant","session_id":"sess-auth","error":"authentication_failed","is_api_error_message":true,"message":{{"role":"assistant","content":[{{"type":"text","text":"Failed to authenticate: OAuth session expired and could not be refreshed"}}]}}}}'
    printf '%s\n' '{{"type":"result","subtype":"success","is_error":true,"num_turns":1,"result":"Failed to authenticate: OAuth session expired and could not be refreshed","session_id":"sess-auth","total_cost_usd":0}}'
    ;;
  "fail-error-result")
    printf '%s\n' '{{"type":"system","subtype":"init","session_id":"sess-err"}}'
    printf '%s\n' '{{"type":"result","subtype":"error_during_execution","is_error":true,"session_id":"sess-err","result":"API Error: 529 overloaded"}}'
    exit 1
    ;;
  "fail-crash")
    echo "fatal crash on stderr" >&2
    exit 42
    ;;
  "hang-cancel")
    sleep 60 &
    grand=$!
    printf '%s\n%s\n' "$$" "$grand" > "$rec/pids.tmp"
    mv "$rec/pids.tmp" "$rec/pids"
    wait
    ;;
  "linger-exit")
    sleep 60 </dev/null >/dev/null 2>&1 &
    grand=$!
    printf '%s\n' "$grand" > "$rec/grand.tmp"
    mv "$rec/grand.tmp" "$rec/grand"
    printf '%s\n' '{{"type":"result","subtype":"success","is_error":false,"session_id":"sess-linger","result":"done before grandchild","total_cost_usd":0.001,"usage":{{"input_tokens":10,"output_tokens":5,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}}'
    exit 0
    ;;
  *)
    echo "unexpected prompt: $prompt" >&2
    exit 99
    ;;
esac
"#,
        rec = shell_quote(&rec),
    );
    burrow_test::executable(path, script);
}
