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
fn live_session_streams_before_outer_deadline_or_kill() {
    use std::io::{BufRead, BufReader};
    use std::sync::mpsc;
    use std::thread;

    let dir = TempDir::new().unwrap();
    let fixture = write_executable(
        dir.path(),
        "ag-live-session",
        "#!/bin/sh\ncat >/dev/null\nprintf '{\"event\":\"init\",\"conversation_id\":\"live-sess-1\"}\\n'\nexec sleep 60\n",
    );
    let cfg = Config {
        binary: fixture,
        models: sample_models(),
        default_model: Some("test-model".to_owned()),
        activity_secs: 60,
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
            prompt: "long turn".to_owned(),
            model: None,
            effort: None,
        },
    };

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
    let (tx, rx) = mpsc::channel::<String>();
    let reader = thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    // The session event must arrive on stderr while the plugin and CLI are still
    // running, before any outer deadline or kill.
    let mut got_session = None;
    let wait_until = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < wait_until {
        let left = wait_until.saturating_duration_since(std::time::Instant::now());
        match rx.recv_timeout(left) {
            Ok(line) => {
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&line)
                    && val["event"] == "session"
                {
                    got_session = val["session_id"].as_str().map(str::to_owned);
                    break;
                }
            }
            Err(_) => break,
        }
    }

    // Verify the plugin is still running when the live session event arrives,
    // then terminate it cleanly via SIGTERM so its child group is reaped too.
    assert!(
        child.try_wait().unwrap().is_none(),
        "plugin exited before live session event check"
    );
    let _ = rustix::process::kill_process(
        rustix::process::Pid::from_child(&child),
        rustix::process::Signal::TERM,
    );
    let _ = child.wait();
    drop(rx);
    let _ = reader.join();

    assert_eq!(
        got_session.as_deref(),
        Some("live-sess-1"),
        "expected live session event on stderr before plugin exits or is killed"
    );
}

#[test]
fn sigterm_cancels_child_group_and_emits_partial_session_and_usage() {
    use rustix::process::{Pid, Signal, kill_process};
    use std::time::Instant;

    let dir = TempDir::new().unwrap();
    let child_pid_file = dir.path().join("child.pid");
    let ready_file = dir.path().join("ready");
    let fixture = write_executable(
        dir.path(),
        "ag-sigterm-stub",
        &format!(
            r#"#!/bin/sh
cat >/dev/null
sleep 60 &
echo $! > "{}"
printf '{{"event":"init","conversation_id":"sigterm-sess-1"}}\n'
printf '{{"event":"step_update","usage":{{"input_tokens":42,"output_tokens":7}}}}\n'
touch "{}"
wait
"#,
            child_pid_file.display(),
            ready_file.display(),
        ),
    );
    let cfg = Config {
        binary: fixture,
        models: sample_models(),
        default_model: Some("test-model".to_owned()),
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
            prompt: "cut by proj deadline".to_owned(),
            model: None,
            effort: None,
        },
    };

    let mut child = Command::new(burrow_test::program!("burrow-engine-antigravity"))
        .arg(&cfg_path)
        .env_remove("PROJ_STREAM_EVENTS")
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

    let ready_deadline = Instant::now() + Duration::from_secs(5);
    while !ready_file.exists() && Instant::now() < ready_deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(ready_file.exists(), "fixture never signaled ready");
    // Give the plugin a moment to finish draining the init/usage lines.
    std::thread::sleep(Duration::from_millis(50));

    let pid = Pid::from_child(&child);
    kill_process(pid, Signal::TERM).unwrap();

    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(1),
        "expected clean EXIT_FAILURE after SIGTERM, got {:?}; stderr: {stderr}",
        output.status
    );
    let diag: serde_json::Value = stderr
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v["event"] == "error")
        .unwrap_or_else(|| panic!("expected structured error diagnostic on stderr: {stderr}"));
    assert_eq!(diag["session"].as_str(), Some("sigterm-sess-1"), "{diag}");
    assert_eq!(
        diag["error"].as_str(),
        Some("antigravity invocation was cancelled"),
        "{diag}"
    );
    assert_eq!(diag["usage"]["input_tokens"].as_u64(), Some(42), "{diag}");
    assert_eq!(diag["usage"]["output_tokens"].as_u64(), Some(7), "{diag}");

    let bg_pid = fs::read_to_string(&child_pid_file).unwrap();
    let bg_pid = bg_pid.trim();
    let gone = (0..200).any(|_| {
        std::thread::sleep(Duration::from_millis(10));
        !Path::new(&format!("/proc/{bg_pid}")).exists()
    });
    assert!(gone, "background child {bg_pid} was not killed on SIGTERM");
}
