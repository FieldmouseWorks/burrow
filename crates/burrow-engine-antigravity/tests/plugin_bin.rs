use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use burrow_engine_antigravity::STRIPPED_ENV;
use burrow_engine_antigravity::config::{
    CONFIG_ENV, Config, CredentialMode, DEFAULT_CREDENTIAL_HOME_PATH, EFFORTS, ENGINE, Effort,
    ModelSpec, OutputFormat,
};
use conary_core::plugin::{
    Answer, CONTRACT, Choice, Context, Execution, Identity, Kind, Limits, Negotiated, Next, Plugin,
    Request, Task,
};
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

#[test]
fn github_tokens_are_stripped_and_isolated_file_credentials_are_cleaned_up() {
    let dir = TempDir::new().unwrap();
    let env_dump = dir.path().join("child-env.txt");
    let cred_path_dump = dir.path().join("cred-path.txt");

    let fixture = write_executable(
        dir.path(),
        "ag-env-check",
        &format!(
            r#"#!/bin/sh
cat >/dev/null
env > "{}"
if [ -n "$AG_CRED_FILE" ] && [ -r "$AG_CRED_FILE" ]; then
  printf '%s\n' "$AG_CRED_FILE" > "{}"
  val=$(cat "$AG_CRED_FILE")
  printf '{{"type":"result","conversation_id":"env-1","result":"cred=%s"}}\n' "$val"
else
  printf '{{"type":"result","conversation_id":"env-1","result":"no-cred"}}\n'
fi
"#,
            env_dump.display(),
            cred_path_dump.display(),
        ),
    );

    let cfg = Config {
        binary: fixture,
        models: sample_models(),
        default_model: Some("test-model".to_owned()),
        credential_env: Some("BURROW_AG_TEST_IN".to_owned()),
        target_credential_env: Some("AG_CRED_FILE".to_owned()),
        credential_mode: CredentialMode::File,
        ..Config::default()
    };
    let cfg_path = dir.path().join("config.json");
    fs::write(&cfg_path, serde_json::to_vec(&cfg).unwrap()).unwrap();

    let req = Request {
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
            workdir: dir.path().to_string_lossy().into_owned(),
            prompt: "check env".to_owned(),
            model: None,
            effort: None,
        },
    };
    let req_bytes = serde_json::to_vec(&req).unwrap();

    let mut child = Command::new(burrow_test::program!("burrow-engine-antigravity"))
        .arg(&cfg_path)
        .env("BURROW_AG_TEST_IN", "isolated-antigravity-secret")
        .env("GH_TOKEN", "ghp_must_never_leak")
        .env("GITHUB_TOKEN", "ghp_must_never_leak_2")
        .env("GH_TOKEN_SOCKET", "/run/gh-token/workers.sock")
        .env("CLAUDE_CODE_OAUTH_TOKEN", "claude_must_never_leak")
        .env("ANTHROPIC_API_KEY", "sk-ant-must-never-leak")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.as_mut().unwrap().write_all(&req_bytes).unwrap();
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let dumped_env = fs::read_to_string(&env_dump).unwrap();
    for forbidden in STRIPPED_ENV {
        assert!(
            !dumped_env
                .lines()
                .any(|l| l.starts_with(&format!("{forbidden}="))),
            "child env leaked {forbidden}:\n{dumped_env}"
        );
    }
    assert!(
        !dumped_env
            .lines()
            .any(|l| l.starts_with("BURROW_AG_TEST_IN=")),
        "source credential env should not leak when target_credential_env differs"
    );

    let cred_path = PathBuf::from(fs::read_to_string(&cred_path_dump).unwrap().trim());
    assert!(
        !cred_path.exists(),
        "temp credential file should be cleaned up after exit"
    );
}

#[test]
fn conary_runner_v5_end_to_end_check_fresh_and_continue() {
    let dir = TempDir::new().unwrap();
    let fixture = write_executable(
        dir.path(),
        "fake-ag",
        r#"#!/bin/sh
cat >/dev/null
conv=""
model=""
effort=""
prev=""
for a in "$@"; do
  case "$prev" in
    --conversation) conv="$a" ;;
    --model) model="$a" ;;
    --effort) effort="$a" ;;
  esac
  prev="$a"
done
if [ -n "$conv" ]; then
  printf '{"type":"result","conversation_id":"%s","result":"cont:%s:%s"}\n' "$conv" "$model" "$effort"
else
  printf '{"type":"result","conversation_id":"ag-sess-9","result":"fresh:%s:%s"}\n' "$model" "$effort"
fi
"#,
    );

    let cfg = Config {
        binary: fixture,
        models: sample_models(),
        default_model: Some("test-model".to_owned()),
        default_effort: Some(Effort::High),
        meters: vec!["quota".to_owned()],
        ..Config::default()
    };
    let cfg_path = dir.path().join("plugin.json");
    fs::write(&cfg_path, serde_json::to_vec(&cfg).unwrap()).unwrap();

    let wrapper = write_executable(
        dir.path(),
        "antigravity-plugin",
        &format!(
            "#!/bin/sh\nexport {CONFIG_ENV}=\"{}\"\nexec \"{}\"\n",
            cfg_path.display(),
            burrow_test::program!("burrow-engine-antigravity"),
        ),
    );
    let limits = Limits {
        deadline: Duration::from_secs(15),
        response_bytes: 1_048_576,
    };

    // 1. Conary's own conformance check passes.
    let plugin = Plugin::describe(&wrapper, limits).unwrap();
    assert_eq!(plugin.identity.engine, ENGINE);
    let checked = plugin.check().unwrap();
    assert_eq!(checked, vec![Kind::Job]);

    // 2. Fresh v5 job through Conary's real runner.
    let fresh_task = Task::Job {
        repo: "owner/demo".to_owned(),
        workdir: dir.path().to_string_lossy().into_owned(),
        prompt: "first turn".to_owned(),
        model: Some("test-model".to_owned()),
        effort: Some("xhigh".to_owned()),
    };
    let fresh = plugin
        .call_with(
            fresh_task,
            Some(Choice {
                engine: ENGINE.to_owned(),
                model: Some("test-model".to_owned()),
                effort: Some("xhigh".to_owned()),
            }),
            Context::Fresh,
            None,
        )
        .unwrap();
    assert_eq!(fresh.by.session.as_deref(), Some("ag-sess-9"));
    assert!(matches!(
        fresh.answer,
        Answer::Job { ref summary, next: Next::Done } if summary == "fresh:test-model:xhigh"
    ));

    // 3. Continued v5 job through Conary's real runner.
    let cont_task = Task::Job {
        repo: "owner/demo".to_owned(),
        workdir: dir.path().to_string_lossy().into_owned(),
        prompt: "second turn".to_owned(),
        model: Some("alt-model".to_owned()),
        effort: Some("max".to_owned()),
    };
    let cont = plugin
        .call_with(
            cont_task,
            Some(Choice {
                engine: ENGINE.to_owned(),
                model: Some("alt-model".to_owned()),
                effort: Some("max".to_owned()),
            }),
            Context::Continue {
                session: "ag-sess-9".to_owned(),
            },
            None,
        )
        .unwrap();
    assert_eq!(cont.by.session.as_deref(), Some("ag-sess-9"));
    assert!(matches!(
        cont.answer,
        Answer::Job { ref summary, next: Next::Done } if summary == "cont:alt-model:max"
    ));
}

#[test]
fn home_file_mode_materializes_oauth_token_strips_envs_and_classifies_login_refusal() {
    let dir = TempDir::new().unwrap();
    let fake_home = TempDir::new().unwrap();
    let env_dump = dir.path().join("home-file-env.txt");
    let token_payload = r#"{"access_token":"ya29.bin-oauth-secret-access","refresh_token":"1//bin-oauth-secret-refresh"}"#;

    let ok_fixture = write_executable(
        dir.path(),
        "ag-home-file-bin",
        &format!(
            r#"#!/bin/sh
cat >/dev/null
env > "{}"
tok="$HOME/{DEFAULT_CREDENTIAL_HOME_PATH}"
if [ -f "$tok" ] && [ -w "$tok" ]; then
  val=$(cat "$tok")
  printf '{{"type":"result","conversation_id":"hf-bin-1","result":"loaded:%s"}}\n' "${{#val}}"
else
  printf '{{"status":"ERROR","error":"missing token"}}\n'
  exit 1
fi
"#,
            env_dump.display()
        ),
    );

    let cfg = Config {
        binary: ok_fixture,
        models: sample_models(),
        default_model: Some("test-model".to_owned()),
        credential_env: Some("BURROW_ANTIGRAVITY_CREDENTIAL".to_owned()),
        target_credential_env: Some("ANTIGRAVITY_API_KEY".to_owned()),
        credential_mode: CredentialMode::HomeFile,
        ..Config::default()
    };
    let cfg_path = dir.path().join("home-file-config.json");
    fs::write(&cfg_path, serde_json::to_vec(&cfg).unwrap()).unwrap();

    let req = Request {
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
            workdir: dir.path().to_string_lossy().into_owned(),
            prompt: "check home-file".to_owned(),
            model: None,
            effort: None,
        },
    };
    let req_bytes = serde_json::to_vec(&req).unwrap();

    let mut child = Command::new(burrow_test::program!("burrow-engine-antigravity"))
        .arg(&cfg_path)
        .env("HOME", fake_home.path())
        .env("BURROW_ANTIGRAVITY_CREDENTIAL", token_payload)
        .env("ANTIGRAVITY_API_KEY", "must-be-stripped-target-env")
        .env("GH_TOKEN", "ghp_must_never_leak")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.as_mut().unwrap().write_all(&req_bytes).unwrap();
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let dumped_env = fs::read_to_string(&env_dump).unwrap();
    for forbidden in [
        "BURROW_ANTIGRAVITY_CREDENTIAL",
        "ANTIGRAVITY_API_KEY",
        "GH_TOKEN",
    ] {
        assert!(
            !dumped_env
                .lines()
                .any(|l| l.starts_with(&format!("{forbidden}="))),
            "child env leaked {forbidden}:\n{dumped_env}"
        );
    }

    let gemini_dir = fake_home.path().join(".gemini");
    let cli_dir = gemini_dir.join("antigravity-cli");
    let tok_path = cli_dir.join("antigravity-oauth-token");
    assert_eq!(
        fs::metadata(&gemini_dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&cli_dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&tok_path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(fs::read_to_string(&tok_path).unwrap(), token_payload);

    // Unauthenticated CLI output classifies as login refusal and scrubs the OAuth secret.
    let unauth_fixture = write_executable(
        dir.path(),
        "ag-unauth-bin",
        r#"#!/bin/sh
echo "Authentication required. Please visit the URL to log in: https://example.test/auth?t=ya29.bin-oauth-secret-access" >&2
printf '{"status":"ERROR","error":"authentication failed or timed out"}\n'
exit 1
"#,
    );
    let mut unauth_cfg = cfg;
    unauth_cfg.binary = unauth_fixture;
    unauth_cfg.output_format = OutputFormat::Json;
    let unauth_cfg_path = dir.path().join("unauth-config.json");
    fs::write(&unauth_cfg_path, serde_json::to_vec(&unauth_cfg).unwrap()).unwrap();

    let mut unauth_child = Command::new(burrow_test::program!("burrow-engine-antigravity"))
        .arg(&unauth_cfg_path)
        .env("HOME", fake_home.path())
        .env("BURROW_ANTIGRAVITY_CREDENTIAL", token_payload)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    unauth_child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(&req_bytes)
        .unwrap();
    drop(unauth_child.stdin.take());
    let unauth_output = unauth_child.wait_with_output().unwrap();
    assert!(!unauth_output.status.success());
    let stderr = String::from_utf8_lossy(&unauth_output.stderr);
    assert!(stderr.contains("antigravity login refusal"), "{stderr}");
    assert!(!stderr.contains("ya29.bin-oauth-secret-access"), "{stderr}");
    assert!(stderr.contains("[redacted]"), "{stderr}");
}

/// What the runner tells the engine for the turn (`PROJ_APPEND_SYSTEM_PROMPT`)
/// follows the task in the prompt the CLI is given, and a run whose runner
/// streams events says it's at work while the CLI, whose output comes only
/// at its end, runs; a runner that doesn't stream hears nothing of it.
#[test]
fn a_job_gets_the_turns_instructions_and_says_its_at_work_while_the_cli_runs() {
    for streams in [true, false] {
        let dir = TempDir::new().unwrap();
        let given = dir.path().join("stdin.txt");
        let fixture = write_executable(
            dir.path(),
            "ag-slow",
            &format!(
                "#!/bin/sh\ncat > \"{}\"\nsleep 3\nprintf '{{\"type\":\"result\",\"conversation_id\":\"slow-1\",\"result\":\"done\"}}\\n'\n",
                given.display()
            ),
        );
        let cfg = Config {
            binary: fixture,
            models: sample_models(),
            default_model: Some("test-model".to_owned()),
            activity_secs: 1,
            ..Config::default()
        };
        let cfg_path = dir.path().join("config.json");
        fs::write(&cfg_path, serde_json::to_vec(&cfg).unwrap()).unwrap();
        let req = Request {
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
                workdir: dir.path().to_string_lossy().into_owned(),
                prompt: "do the task".to_owned(),
                model: None,
                effort: None,
            },
        };
        let mut command = Command::new(burrow_test::program!("burrow-engine-antigravity"));
        command
            .arg(&cfg_path)
            .env("PROJ_APPEND_SYSTEM_PROMPT", "End by running proj done.")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if streams {
            command.env("PROJ_STREAM_EVENTS", "1");
        } else {
            command.env_remove("PROJ_STREAM_EVENTS");
        }
        let mut child = command.spawn().unwrap();
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(&serde_json::to_vec(&req).unwrap())
            .unwrap();
        drop(child.stdin.take());
        let output = child.wait_with_output().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "stderr: {stderr}");
        let given_msg: serde_json::Value =
            serde_json::from_str(fs::read_to_string(&given).unwrap().trim()).unwrap();
        assert_eq!(
            given_msg,
            serde_json::json!({
                "event": "user",
                "message": { "content": "do the task\n\nEnd by running proj done." }
            })
        );
        let said = stderr
            .lines()
            .filter(|l| *l == r#"{"event":"activity"}"#)
            .count();
        if streams {
            assert!(said >= 2, "{said} activity events in a 3s run: {stderr}");
        } else {
            assert_eq!(said, 0, "{stderr}");
        }
    }
}

#[test]
fn real_antigravity_stream_json_envelopes_normalize_result_schema_errors_and_leading_dash_prompt() {
    use burrow_engine_antigravity::{
        Invocation, Refusal, RunError, Schema, build_args, normalize_output,
    };

    // 1a. Default `stream-json` uses `--input-format stream-json` + `--print=` (never bare `-p` before `--output-format`).
    let stream_inv = Invocation {
        workdir: PathBuf::from("/tmp/work"),
        prompt: "--leading-dash prompt".to_owned(),
        model: Some("test-model".to_owned()),
        effort: Some(Effort::High),
        conversation: None,
        add_dirs: Vec::new(),
        json_schema: None,
        mode: None,
        dangerously_skip_permissions: false,
        sandbox: false,
        disable_slash_commands: true,
        settings_file: None,
        output_format: OutputFormat::StreamJson,
        input_format: None,
        print_timeout_secs: None,
        home_dir: None,
    };
    let stream_args: Vec<String> = build_args(&stream_inv)
        .into_iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        stream_args.first().map(String::as_str),
        Some("--output-format")
    );
    assert!(
        stream_args
            .windows(2)
            .any(|w| w == ["--input-format", "stream-json"])
    );
    assert_eq!(stream_args.last().map(String::as_str), Some("--print="));
    assert!(!stream_args.iter().any(|a| a == "-p"));

    // 1b. Single-shot `json` mode attaches a leading-`-` prompt to `--print=` at the end.
    let mut dash_inv = stream_inv;
    dash_inv.output_format = OutputFormat::Json;
    let args: Vec<String> = build_args(&dash_inv)
        .into_iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(args.first().map(String::as_str), Some("--output-format"));
    assert_eq!(
        args.last().map(String::as_str),
        Some("--print=--leading-dash prompt")
    );
    assert!(!args.iter().any(|a| a == "-p"));

    // 2. Real `stream-json` envelope with `init`, recoverable tool `step_update` error, and `result`.
    let real_stream = concat!(
        r#"{"event":"init","conversation_id":"conv-real-1","init":{"model":"test-model","cwd":"/tmp/work","permission_mode":"request-review"}}"#,
        "\n",
        r#"{"event":"step_update","step_update":{"conversation_id":"conv-real-1","step_index":0,"state":"DONE","step_type":"user_input"}}"#,
        "\n",
        r#"{"event":"step_update","step_update":{"conversation_id":"conv-real-1","step_index":1,"state":"ERROR","step_type":"tool","tool_name":"run_command","tool_info":{"error":{"type":"TOOL_ERROR","message":"recoverable"}}}}"#,
        "\n",
        r#"{"event":"step_update","step_update":{"conversation_id":"conv-real-1","step_index":2,"state":"DONE","step_type":"agent_response","text_delta":"OK\n"}}"#,
        "\n",
        r#"{"event":"result","result":{"conversation_id":"conv-real-1","status":"SUCCESS","response":"OK\n","duration_seconds":2.5,"num_turns":1,"usage":{"input_tokens":120,"output_tokens":15,"cache_read_tokens":40,"total_tokens":135}}}"#,
        "\n"
    );
    let ok = normalize_output(
        OutputFormat::StreamJson,
        real_stream.as_bytes(),
        b"",
        Some(0),
        None,
        None,
        &[],
    );
    assert_eq!(ok.error, None, "{ok:?}");
    assert_eq!(ok.session.as_deref(), Some("conv-real-1"));
    assert_eq!(ok.result.as_deref(), Some("OK\n"));
    let usage = ok.usage.expect("usage from real stream-json");
    assert_eq!(usage.model.as_deref(), Some("test-model"));
    assert_eq!(usage.input_tokens, Some(120));
    assert_eq!(usage.output_tokens, Some(15));
    assert_eq!(usage.cache_read_tokens, Some(40));
    assert_eq!(usage.total_tokens, Some(135));

    // 3. Real `--json-schema` structured output inside `event: "result"`.
    let schema_stream = concat!(
        r#"{"event":"init","conversation_id":"conv-schema-1","init":{"model":"test-model"}}"#,
        "\n",
        r#"{"event":"result","result":{"conversation_id":"conv-schema-1","status":"SUCCESS","response":"{\"verdict\":\"pass\",\"reason\":\"ok\"}\n","structured_output":{"verdict":"pass","reason":"ok"},"usage":{"input_tokens":90,"output_tokens":12,"total_tokens":102}}}"#,
        "\n"
    );
    let schema = Schema::compile(r#"{"type":"object","required":["verdict","reason"]}"#).unwrap();
    let structured = normalize_output(
        OutputFormat::StreamJson,
        schema_stream.as_bytes(),
        b"",
        Some(0),
        None,
        Some(&schema),
        &[],
    );
    assert_eq!(structured.error, None, "{structured:?}");
    assert_eq!(structured.session.as_deref(), Some("conv-schema-1"));
    assert_eq!(
        structured.structured_output,
        Some(serde_json::json!({"verdict": "pass", "reason": "ok"}))
    );

    // 4. Wrapped `status: "ERROR"` in `event: "result"` fails even on exit 0 and classifies refusals.
    let auth_err = concat!(
        r#"{"event":"result","result":{"conversation_id":"","status":"ERROR","response":"","error":"authentication failed or timed out","finished_reason":"error"}}"#,
        "\n"
    );
    let auth_out = normalize_output(
        OutputFormat::StreamJson,
        auth_err.as_bytes(),
        b"",
        Some(0),
        None,
        None,
        &[],
    );
    assert!(
        matches!(
            auth_out.error,
            Some(RunError::Refusal(Refusal::Login { .. }))
        ),
        "{auth_out:?}"
    );

    let quota_err = concat!(
        r#"{"event":"result","result":{"conversation_id":"","status":"ERROR","response":"","error":"RESOURCE_EXHAUSTED: quota exceeded","finished_reason":"error"}}"#,
        "\n"
    );
    let quota_out = normalize_output(
        OutputFormat::StreamJson,
        quota_err.as_bytes(),
        b"",
        Some(1),
        None,
        None,
        &[],
    );
    assert!(
        matches!(
            quota_out.error,
            Some(RunError::Refusal(Refusal::Quota { .. }))
        ),
        "{quota_out:?}"
    );

    // 5. Empty response with `denied_actions` surfaces the denied action in InvalidOutput.
    let denied = concat!(
        r#"{"event":"result","result":{"conversation_id":"conv-deny-1","status":"SUCCESS","response":"","denied_actions":[{"action":"command","display_name":"Bash"}]}}"#,
        "\n"
    );
    let denied_out = normalize_output(
        OutputFormat::StreamJson,
        denied.as_bytes(),
        b"jetski: command permission auto-denied\n",
        Some(0),
        None,
        None,
        &[],
    );
    assert!(
        matches!(
            denied_out.error,
            Some(RunError::InvalidOutput(ref msg)) if msg.contains("denied actions: command")
        ),
        "{denied_out:?}"
    );
}

#[test]
fn host_mode_persists_isolated_home_across_turns_in_proj_scratch() {
    let dir = TempDir::new().unwrap();
    let scratch = dir.path().join("proj-scratch");
    fs::create_dir_all(&scratch).unwrap();
    let fake_real_home = dir.path().join("real-home");
    fs::create_dir_all(fake_real_home.join(".cargo")).unwrap();
    fs::write(fake_real_home.join(".gitconfig"), "[user]\nname = Test\n").unwrap();

    // Fake `antigravity` CLI:
    // - On fresh turn (no `--conversation`): creates `$HOME/.gemini/antigravity-cli/session-sess-1`
    //   and returns `conversation_id: "sess-1"`.
    // - On continue turn (`--conversation sess-1`): checks that the session file
    //   exists in `$HOME`; if missing, starts a new `sess-2` (reproducing the
    //   per-turn TempDir bug where `plugin.rs` rejects the changed session).
    let fixture = write_executable(
        dir.path(),
        "ag-session-persist",
        r#"#!/bin/sh
cat >/dev/null
conv=""
prev=""
for arg in "$@"; do
  if [ "$prev" = "--conversation" ]; then
    conv="$arg"
  fi
  prev="$arg"
done
state_dir="$HOME/.gemini/antigravity-cli"
mkdir -p "$state_dir"
if [ -z "$conv" ]; then
  printf 'turn1\n' > "$state_dir/session-sess-1"
  printf '{"type":"result","conversation_id":"sess-1","result":"turn 1 ok"}\n'
elif [ -f "$state_dir/session-$conv" ]; then
  printf '{"type":"result","conversation_id":"%s","result":"turn 2 ok;git=%s;cargo=%s"}\n' \
    "$conv" "${GIT_CONFIG_GLOBAL:-}" "${CARGO_HOME:-}"
else
  printf '{"type":"result","conversation_id":"sess-2","result":"lost session"}\n'
fi
"#,
    );

    let cfg = Config {
        binary: fixture,
        models: sample_models(),
        default_model: Some("test-model".to_owned()),
        host_mode: true,
        credential_mode: CredentialMode::HomeFile,
        credential_env: Some("BURROW_AG_TEST_IN".to_owned()),
        ..Config::default()
    };
    let cfg_path = dir.path().join("config.json");
    fs::write(&cfg_path, serde_json::to_vec(&cfg).unwrap()).unwrap();

    let run_turn = |context: Context| -> std::process::Output {
        let req = Request {
            contract: CONTRACT.to_owned(),
            task: Task::Job {
                repo: "owner/demo".to_owned(),
                prompt: "continue check".to_owned(),
                workdir: dir.path().to_string_lossy().into_owned(),
                model: Some("test-model".to_owned()),
                effort: Some("low".to_owned()),
            },
            negotiated: Some(Negotiated {
                by: Identity {
                    engine: ENGINE.to_owned(),
                    session: None,
                },
                offers: cfg.offers(),
            }),
            execution: Some(Execution {
                choice: Some(Choice {
                    engine: ENGINE.to_owned(),
                    model: Some("test-model".to_owned()),
                    effort: Some("low".to_owned()),
                }),
                context,
            }),
        };
        let mut child = Command::new(burrow_test::program!("burrow-engine-antigravity"))
            .env(CONFIG_ENV, &cfg_path)
            .env("HOME", &fake_real_home)
            .env("PROJ_SCRATCH", &scratch)
            .env("BURROW_AG_TEST_IN", "secret-token")
            .env_remove("GIT_CONFIG_GLOBAL")
            .env_remove("CARGO_HOME")
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
        child.wait_with_output().unwrap()
    };

    let out1 = run_turn(Context::Fresh);
    assert!(
        out1.status.success(),
        "turn 1 failed: {}",
        String::from_utf8_lossy(&out1.stderr)
    );
    let resp1: conary_core::plugin::Response = serde_json::from_slice(&out1.stdout).unwrap();
    assert_eq!(resp1.by.session.as_deref(), Some("sess-1"));
    assert!(
        !fake_real_home.join(".gemini").exists(),
        "real HOME must remain untouched"
    );

    let out2 = run_turn(Context::Continue {
        session: "sess-1".to_owned(),
    });
    assert!(
        out2.status.success(),
        "turn 2 failed: {}",
        String::from_utf8_lossy(&out2.stderr)
    );
    let resp2: conary_core::plugin::Response = serde_json::from_slice(&out2.stdout).unwrap();
    assert_eq!(resp2.by.session.as_deref(), Some("sess-1"));
    let expected_git = fake_real_home.join(".gitconfig");
    let expected_cargo = fake_real_home.join(".cargo");
    match resp2.answer {
        Answer::Job { summary, .. } => {
            assert_eq!(
                summary,
                format!(
                    "turn 2 ok;git={};cargo={}",
                    expected_git.display(),
                    expected_cargo.display()
                )
            );
        }
        other => panic!("unexpected answer: {other:?}"),
    }
}
