use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use conary_core::plugin::{
    Answer, CONTRACT, CONTRACT_V1, Choice, Context, Execution, Identity, Kind, Negotiated, Next,
    Request, Sandbox, Task, fixtures,
};
use tempfile::TempDir;

use crate::cli::{Invocation, build_args, run_invocation};
use crate::config::{Config, EFFORTS, ENGINE, Effort, InputFormat, Mode, ModelSpec, OutputFormat};
use crate::normalize::{Refusal, RunError, normalize_output};
use crate::plugin::{answer, answer_cancellable, describe};
use crate::schema::Schema;

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

fn base_config(binary: PathBuf) -> Config {
    Config {
        binary,
        models: sample_models(),
        default_model: Some("test-model".to_owned()),
        default_effort: Some(Effort::Medium),
        deadline_secs: 30,
        meters: vec!["quota".to_owned()],
        ..Config::default()
    }
}

fn v5_job_request(
    config: &Config,
    workdir: &Path,
    prompt: &str,
    model: Option<&str>,
    effort: Option<&str>,
    context: Context,
) -> Request {
    let choice = (model.is_some() || effort.is_some()).then(|| Choice {
        engine: ENGINE.to_owned(),
        model: model.map(str::to_owned),
        effort: effort.map(str::to_owned),
    });
    Request {
        contract: CONTRACT.to_owned(),
        execution: Some(Execution { choice, context }),
        negotiated: Some(Negotiated {
            by: Identity {
                engine: ENGINE.to_owned(),
                session: None,
            },
            offers: config.offers(),
        }),
        task: Task::Job {
            repo: "owner/demo".to_owned(),
            workdir: workdir.to_string_lossy().into_owned(),
            prompt: prompt.to_owned(),
            model: model.map(str::to_owned),
            effort: effort.map(str::to_owned),
        },
    }
}

#[test]
fn describe_reports_v5_runtime_offers_and_capabilities() {
    let cfg = Config {
        models: sample_models(),
        default_model: Some("test-model".to_owned()),
        default_effort: Some(Effort::High),
        network: true,
        devices: vec!["kvm".to_owned()],
        meters: vec!["quota".to_owned()],
        ..Config::default()
    };
    let resp = describe(&cfg);
    assert_eq!(resp.contract, CONTRACT_V1);
    assert_eq!(resp.by.engine, ENGINE);
    assert_eq!(resp.by.session, None);
    let Answer::Describe {
        versions,
        kinds,
        v2_kinds,
        v4_kinds,
        models,
        capabilities,
        meters,
    } = resp.answer
    else {
        panic!("expected Describe answer");
    };
    assert!(versions.iter().any(|v| v == CONTRACT));
    assert!(kinds.is_empty());
    assert_eq!(v2_kinds, vec![Kind::Job]);
    assert!(v4_kinds.is_empty());
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].id, "test-model");
    assert_eq!(models[0].efforts, EFFORTS);
    assert_eq!(models[1].id, "alt-model");
    let caps = capabilities.expect("capabilities");
    assert_eq!(caps.sandbox, Sandbox::Needs);
    assert!(caps.network);
    assert!(!caps.mcp);
    assert_eq!(caps.devices, vec!["kvm"]);
    assert_eq!(meters, vec!["quota"]);
}

#[test]
fn config_validation_rejects_invalid_models_and_efforts() {
    let mut cfg = Config {
        models: vec![ModelSpec {
            id: "test-model".to_owned(),
            efforts: vec!["turbo".to_owned()],
            context_tokens: None,
            max_output_tokens: None,
        }],
        ..Config::default()
    };
    assert!(cfg.validate().unwrap_err().contains("unsupported effort"));

    cfg.models = vec![
        ModelSpec {
            id: "dup-model".to_owned(),
            efforts: vec!["low".to_owned()],
            context_tokens: None,
            max_output_tokens: None,
        },
        ModelSpec {
            id: "dup-model".to_owned(),
            efforts: vec!["high".to_owned()],
            context_tokens: None,
            max_output_tokens: None,
        },
    ];
    assert!(cfg.validate().unwrap_err().contains("duplicate model"));

    cfg.models.pop();
    cfg.default_model = Some("missing-model".to_owned());
    assert!(cfg.validate().unwrap_err().contains("default_model"));

    cfg.default_model = Some("dup-model".to_owned());
    cfg.default_effort = Some(Effort::Max);
    assert!(cfg.validate().unwrap_err().contains("default_effort"));
}

#[test]
fn build_args_covers_verified_headless_flags() {
    let inv = Invocation {
        workdir: PathBuf::from("/tmp/work"),
        prompt: "hello".to_owned(),
        model: Some("test-model".to_owned()),
        effort: Some(Effort::Xhigh),
        conversation: Some("conv-42".to_owned()),
        add_dirs: vec![PathBuf::from("/tmp/a"), PathBuf::from("/tmp/b")],
        json_schema: Some(r#"{"type":"object"}"#.to_owned()),
        mode: Some(Mode::Plan),
        dangerously_skip_permissions: true,
        sandbox: true,
        disable_slash_commands: true,
        settings_file: Some(PathBuf::from("/etc/antigravity/settings.json")),
        output_format: OutputFormat::Json,
        input_format: Some(InputFormat::Text),
        print_timeout_secs: Some(120),
        home_dir: None,
    };
    let args: Vec<String> = build_args(&inv)
        .into_iter()
        .map(|s| s.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        args,
        vec![
            "-p",
            "--output-format",
            "json",
            "--input-format",
            "text",
            "--disable-slash-commands",
            "--model",
            "test-model",
            "--effort",
            "xhigh",
            "--conversation",
            "conv-42",
            "--add-dir",
            "/tmp/a",
            "--add-dir",
            "/tmp/b",
            "--json-schema",
            r#"{"type":"object"}"#,
            "--mode",
            "plan",
            "--dangerously-skip-permissions",
            "--sandbox",
            "--settings",
            "/etc/antigravity/settings.json",
            "--print-timeout",
            "120",
        ]
    );
}

#[test]
fn conformance_fixture_passes_offline_with_and_without_models() {
    let dir = TempDir::new().unwrap();
    // Binary path intentionally non-executable: conformance must never launch it.
    let missing = dir.path().join("does-not-exist");

    for models in [sample_models(), Vec::new()] {
        let cfg = Config {
            binary: missing.clone(),
            models: models.clone(),
            default_model: models.first().map(|m| m.id.clone()),
            ..Config::default()
        };
        let fixture = fixtures()
            .into_iter()
            .find(|f| matches!(f.task, Task::Job { .. }))
            .unwrap();
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
            task: fixture.task.clone(),
        };
        let resp = answer(&cfg, &req).unwrap();
        assert!(fixture.conforms(&resp.answer));
        assert_eq!(resp.by.engine, ENGINE);
        let executed = resp.execution.expect("v5 execution provenance");
        if models.is_empty() {
            assert_eq!(executed.model, None);
        } else {
            assert_eq!(executed.model.as_deref(), Some("test-model"));
        }
    }
}

#[test]
fn fresh_job_and_continuation_fixture_verify_flags_and_normalization() {
    let dir = TempDir::new().unwrap();
    let args_log = dir.path().join("args.log");
    let stdin_log = dir.path().join("stdin.log");
    let fixture = write_executable(
        dir.path(),
        "fake-antigravity",
        &format!(
            r#"#!/bin/sh
printf '%s\n' "$@" > "{}"
cat > "{}"
conv=""
prev=""
for a in "$@"; do
  if [ "$prev" = "--conversation" ]; then conv="$a"; fi
  prev="$a"
done
if [ -n "$conv" ]; then
  printf '{{"type":"system","conversation_id":"%s"}}\n' "$conv"
  printf '{{"type":"result","conversation_id":"%s","result":"continued reply","usage":{{"input_tokens":11,"output_tokens":7,"cost_usd":0.02}},"model":"alt-model"}}\n' "$conv"
else
  printf '{{"type":"system","conversation_id":"conv-fresh-1"}}\n'
  printf '{{"type":"assistant","text":"fresh "}}\n'
  printf '{{"type":"result","conversation_id":"conv-fresh-1","result":"fresh reply","usage":{{"input_tokens":20,"output_tokens":9,"cache_read_input_tokens":4,"cost_usd":0.05}},"model":"test-model"}}\n'
fi
"#,
            args_log.display(),
            stdin_log.display(),
        ),
    );

    let mut cfg = base_config(fixture);
    cfg.add_dirs = vec![dir.path().join("extra")];
    cfg.mode = Some(Mode::Auto);
    cfg.dangerously_skip_permissions = true;

    // 1. Fresh job using runtime defaults (default_model = test-model, default_effort = medium).
    let fresh_req = v5_job_request(
        &cfg,
        dir.path(),
        "do the fresh job",
        None,
        None,
        Context::Fresh,
    );
    let fresh_resp = answer(&cfg, &fresh_req).unwrap();
    assert_eq!(fresh_resp.by.engine, ENGINE);
    assert_eq!(fresh_resp.by.session.as_deref(), Some("conv-fresh-1"));
    let fresh_exec = fresh_resp.execution.unwrap();
    assert_eq!(fresh_exec.model.as_deref(), Some("test-model"));
    assert_eq!(fresh_exec.effort.as_deref(), Some("medium"));
    assert_eq!(fresh_exec.context, Context::Fresh);
    assert!(matches!(
        fresh_resp.answer,
        Answer::Job { ref summary, next: Next::Done } if summary == "fresh reply"
    ));
    let recorded_args = fs::read_to_string(&args_log).unwrap();
    assert!(recorded_args.contains("-p\n"));
    assert!(recorded_args.contains("--output-format\nstream-json\n"));
    assert!(recorded_args.contains("--disable-slash-commands\n"));
    assert!(recorded_args.contains("--model\ntest-model\n"));
    assert!(recorded_args.contains("--effort\nmedium\n"));
    assert!(recorded_args.contains("--dangerously-skip-permissions\n"));
    assert!(!recorded_args.contains("--conversation"));
    assert_eq!(fs::read_to_string(&stdin_log).unwrap(), "do the fresh job");

    // Verify normalized TurnOutput directly as well (usage + session + result).
    let inv = Invocation::from_config(
        &cfg,
        dir.path().to_path_buf(),
        "usage check".to_owned(),
        Some("test-model".to_owned()),
        Some(Effort::High),
        None,
    );
    let turn = run_invocation(&cfg, &inv, &|| false);
    assert_eq!(turn.error, None);
    assert_eq!(turn.session.as_deref(), Some("conv-fresh-1"));
    assert_eq!(turn.result.as_deref(), Some("fresh reply"));
    let usage = turn.usage.expect("normalized usage");
    assert_eq!(usage.input_tokens, Some(20));
    assert_eq!(usage.output_tokens, Some(9));
    assert_eq!(usage.cache_read_tokens, Some(4));
    assert_eq!(usage.total_tokens, Some(29));
    assert_eq!(usage.cost_usd, Some(0.05));
    assert_eq!(usage.model.as_deref(), Some("test-model"));

    // 2. Continuation job with explicit runtime model + effort selection.
    let cont_req = v5_job_request(
        &cfg,
        dir.path(),
        "continue the job",
        Some("alt-model"),
        Some("max"),
        Context::Continue {
            session: "conv-fresh-1".to_owned(),
        },
    );
    let cont_resp = answer(&cfg, &cont_req).unwrap();
    assert_eq!(cont_resp.by.session.as_deref(), Some("conv-fresh-1"));
    let cont_exec = cont_resp.execution.unwrap();
    assert_eq!(cont_exec.model.as_deref(), Some("alt-model"));
    assert_eq!(cont_exec.effort.as_deref(), Some("max"));
    assert_eq!(
        cont_exec.context,
        Context::Continue {
            session: "conv-fresh-1".to_owned()
        }
    );
    assert!(matches!(
        cont_resp.answer,
        Answer::Job { ref summary, next: Next::Done } if summary == "continued reply"
    ));
    let recorded_cont_args = fs::read_to_string(&args_log).unwrap();
    assert!(recorded_cont_args.contains("--conversation\nconv-fresh-1\n"));
    assert!(recorded_cont_args.contains("--model\nalt-model\n"));
    assert!(recorded_cont_args.contains("--effort\nmax\n"));

    // 3. Fork context is explicitly rejected.
    let fork_req = v5_job_request(
        &cfg,
        dir.path(),
        "fork attempt",
        Some("test-model"),
        Some("low"),
        Context::Fork {
            session: "conv-fresh-1".to_owned(),
        },
    );
    assert!(
        answer(&cfg, &fork_req)
            .unwrap_err()
            .contains("does not support fork")
    );
}

#[test]
fn structured_json_schema_output_is_passed_and_validated() {
    let dir = TempDir::new().unwrap();
    let args_log = dir.path().join("schema-args.log");
    let ok_fixture = write_executable(
        dir.path(),
        "ag-schema-ok",
        &format!(
            r#"#!/bin/sh
printf '%s\n' "$@" > "{}"
cat >/dev/null
printf '{{"conversation_id":"judge-1","structured_output":{{"verdict":"pass","why":"checks out"}},"usage":{{"promptTokenCount":50,"candidatesTokenCount":12}}}}\n'
"#,
            args_log.display()
        ),
    );
    let schema = r#"{"type":"object","required":["verdict","why"]}"#;
    let mut cfg = base_config(ok_fixture);
    cfg.output_format = OutputFormat::Json;
    cfg.mode = Some(Mode::Plan);
    cfg.json_schema = Some(schema.to_owned());

    let inv = Invocation::from_config(
        &cfg,
        dir.path().to_path_buf(),
        "judge this change".to_owned(),
        Some("test-model".to_owned()),
        Some(Effort::Xhigh),
        None,
    );
    let turn = run_invocation(&cfg, &inv, &|| false);
    assert_eq!(turn.error, None);
    assert_eq!(turn.session.as_deref(), Some("judge-1"));
    assert_eq!(
        turn.structured_output,
        Some(serde_json::json!({"verdict": "pass", "why": "checks out"}))
    );
    let args = fs::read_to_string(&args_log).unwrap();
    assert!(args.contains("--json-schema\n"));
    assert!(args.contains(&format!("{schema}\n")));
    assert!(args.contains("--mode\nplan\n"));
    assert!(args.contains("--output-format\njson\n"));

    // Non-JSON text when --json-schema was requested fails schema validation.
    let bad_fixture = write_executable(
        dir.path(),
        "ag-schema-bad",
        "#!/bin/sh\ncat >/dev/null\nprintf '{\"conversation_id\":\"judge-2\",\"result\":\"plain prose, not json\"}\\n'\n",
    );
    cfg.binary = bad_fixture;
    let bad_turn = run_invocation(&cfg, &inv, &|| false);
    assert!(matches!(
        bad_turn.error,
        Some(RunError::InvalidSchemaOutput(_))
    ));
}

#[test]
fn quota_and_login_refusals_and_process_failures_are_classified_and_scrubbed() {
    let dir = TempDir::new().unwrap();
    let cred_file = dir.path().join("antigravity.cred");
    fs::write(&cred_file, "ag-secret-token-9988776655\n").unwrap();

    // 1. Quota refusal with structured reset timestamp.
    let quota_fixture = write_executable(
        dir.path(),
        "ag-quota",
        r#"#!/bin/sh
cat >/dev/null
printf '{"type":"error","is_error":true,"error":{"code":"RESOURCE_EXHAUSTED","message":"Quota exceeded for model","resets_at":1800000000}}\n'
exit 1
"#,
    );
    let mut cfg = base_config(quota_fixture);
    cfg.credential_file = Some(cred_file.clone());
    cfg.target_credential_env = Some("AG_TEST_CRED".to_owned());
    let inv = Invocation::from_config(
        &cfg,
        dir.path().to_path_buf(),
        "work".to_owned(),
        Some("test-model".to_owned()),
        Some(Effort::Low),
        None,
    );
    let quota_turn = run_invocation(&cfg, &inv, &|| false);
    assert_eq!(
        quota_turn.error,
        Some(RunError::Refusal(Refusal::Quota {
            reset_at: Some(1_800_000_000),
            detail: "Quota exceeded for model".to_owned(),
        }))
    );
    let quota_req = v5_job_request(&cfg, dir.path(), "work", None, None, Context::Fresh);
    let err_msg = answer(&cfg, &quota_req).unwrap_err();
    assert!(err_msg.contains("antigravity quota refusal"));
    assert!(err_msg.contains("1800000000"));

    // 2. Login refusal that echoes the secret token on stderr: classified AND scrubbed.
    let login_fixture = write_executable(
        dir.path(),
        "ag-login",
        r#"#!/bin/sh
cat >/dev/null
printf '{"type":"result","is_error":true,"error":"UNAUTHENTICATED","result":"Authentication failed for token ag-secret-token-9988776655"}\n'
exit 1
"#,
    );
    cfg.binary = login_fixture;
    let login_turn = run_invocation(&cfg, &inv, &|| false);
    match login_turn.error {
        Some(RunError::Refusal(Refusal::Login { detail })) => {
            assert!(!detail.contains("ag-secret-token-9988776655"), "{detail}");
            assert!(detail.contains("[redacted]"), "{detail}");
        }
        other => panic!("expected scrubbed Login refusal, got {other:?}"),
    }

    // 3. Ordinary nonzero exit failure.
    let fail_fixture = write_executable(
        dir.path(),
        "ag-fail",
        "#!/bin/sh\ncat >/dev/null\necho 'internal crash' >&2\nexit 3\n",
    );
    cfg.binary = fail_fixture;
    let fail_turn = run_invocation(&cfg, &inv, &|| false);
    assert_eq!(
        fail_turn.error,
        Some(RunError::ProcessFailed {
            exit_code: Some(3),
            signal: None,
            message: "internal crash".to_owned(),
        })
    );

    // 4. Malformed stdout on exit 0.
    let bad_json = normalize_output(
        OutputFormat::StreamJson,
        b"not json at all\n",
        b"",
        Some(0),
        None,
        None,
        &[],
    );
    assert!(matches!(bad_json.error, Some(RunError::InvalidOutput(_))));
}

#[test]
fn cancellation_kills_entire_process_group_promptly() {
    let dir = TempDir::new().unwrap();
    let child_pid_file = dir.path().join("descendant.pid");
    let fixture = write_executable(
        dir.path(),
        "ag-hang",
        &format!(
            r#"#!/bin/sh
cat >/dev/null
sleep 30 &
echo $! > "{}"
wait
"#,
            child_pid_file.display()
        ),
    );
    let cfg = base_config(fixture);
    let req = v5_job_request(&cfg, dir.path(), "hang", None, None, Context::Fresh);

    let cancelled = Arc::new(AtomicBool::new(false));
    let cancel_check = cancelled.clone();
    let pid_watch = child_pid_file.clone();
    let started = Instant::now();
    let err = answer_cancellable(&cfg, &req, &move || {
        if fs::read_to_string(&pid_watch).is_ok_and(|p| p.ends_with('\n')) {
            cancel_check.store(true, Ordering::SeqCst);
        }
        cancel_check.load(Ordering::SeqCst)
    })
    .unwrap_err();

    assert!(err.contains("cancelled"), "{err}");
    assert!(started.elapsed() < Duration::from_secs(10));

    let descendant_pid = fs::read_to_string(&child_pid_file).unwrap();
    let proc_path = PathBuf::from(format!("/proc/{}", descendant_pid.trim()));
    let gone = (0..200).any(|_| {
        if !proc_path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
        false
    });
    assert!(
        gone,
        "descendant process {} survived process-group cancellation",
        descendant_pid.trim()
    );
}

const VERDICT_SCHEMA: &str = r#"{"type":"object","required":["verdict","why"],"properties":{"verdict":{"type":"string"},"why":{"type":"string"}}}"#;

fn normalize_with(format: OutputFormat, stdout: &str, schema: &Schema) -> crate::TurnOutput {
    normalize_output(
        format,
        stdout.as_bytes(),
        b"",
        Some(0),
        None,
        Some(schema),
        &["sekrit-value"],
    )
}

#[test]
fn structured_output_that_breaks_the_schema_is_refused() {
    let schema = Schema::compile(VERDICT_SCHEMA).unwrap();
    let cases = [
        // An explicit structured_output missing every required field.
        (
            OutputFormat::StreamJson,
            "{\"conversation_id\":\"c\",\"type\":\"result\",\"structured_output\":{}}\n",
        ),
        (OutputFormat::Json, r#"{"structured_output":{}}"#),
        // Result text that is JSON but misses a required field.
        (OutputFormat::Json, r#"{"result":"{\"verdict\":\"pass\"}"}"#),
        // Result text with a wrongly typed field.
        (
            OutputFormat::StreamJson,
            "{\"type\":\"result\",\"result\":\"{\\\"verdict\\\":1,\\\"why\\\":\\\"x\\\"}\"}\n",
        ),
        // Plain text output, valid JSON, wrong shape.
        (
            OutputFormat::Text,
            r#"{"verdict":"pass","why":["sekrit-value"]}"#,
        ),
    ];
    for (format, stdout) in cases {
        let turn = normalize_with(format, stdout, &schema);
        let Some(RunError::InvalidSchemaOutput(why)) = &turn.error else {
            panic!("{format:?} {stdout}: expected a schema refusal, got {turn:?}");
        };
        assert!(!why.contains("sekrit-value"), "unscrubbed: {why}");
        assert_eq!(turn.structured_output, None, "{format:?} {stdout}");
    }
}

#[test]
fn structured_output_that_follows_the_schema_passes() {
    let schema = Schema::compile(VERDICT_SCHEMA).unwrap();
    let want = serde_json::json!({"verdict": "pass", "why": "ok"});
    for (format, stdout) in [
        (
            OutputFormat::StreamJson,
            "{\"type\":\"result\",\"structured_output\":{\"verdict\":\"pass\",\"why\":\"ok\"}}\n",
        ),
        (
            OutputFormat::Json,
            r#"{"result":"{\"verdict\":\"pass\",\"why\":\"ok\"}"}"#,
        ),
        (OutputFormat::Text, r#"{"verdict":"pass","why":"ok"}"#),
    ] {
        let turn = normalize_with(format, stdout, &schema);
        assert_eq!(turn.error, None, "{format:?} {stdout}");
        assert_eq!(turn.structured_output.as_ref(), Some(&want));
    }
}

#[test]
fn a_schema_given_as_a_file_is_held_to() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("schema.json");
    fs::write(&path, VERDICT_SCHEMA).unwrap();
    let schema = Schema::compile(path.to_str().unwrap()).unwrap();
    assert!(schema.validate(&serde_json::json!({})).is_err());
    assert!(
        schema
            .validate(&serde_json::json!({"verdict": "pass", "why": "ok"}))
            .is_ok()
    );
}

#[test]
fn an_unusable_schema_is_refused_before_the_cli_runs() {
    let dir = TempDir::new().unwrap();
    let ran = dir.path().join("ran");
    let fixture = write_executable(
        dir.path(),
        "ag-never",
        &format!(
            "#!/bin/sh\ntouch \"{}\"\nprintf '{{\"structured_output\":{{}}}}\\n'\n",
            ran.display()
        ),
    );
    for bad in [
        r#"{"type":"no-such-type"}"#,
        "/no/such/schema.json",
        "not json, not a path",
    ] {
        assert!(Schema::compile(bad).is_err(), "{bad}");
        let mut cfg = base_config(fixture.clone());
        cfg.json_schema = Some(bad.to_owned());
        assert!(cfg.validate().is_err(), "config took {bad}");
        let inv = Invocation::from_config(
            &cfg,
            dir.path().to_path_buf(),
            "judge".to_owned(),
            None,
            None,
            None,
        );
        let turn = run_invocation(&cfg, &inv, &|| false);
        assert!(
            matches!(turn.error, Some(RunError::ProcessFailed { .. })),
            "{bad}: {turn:?}"
        );
        assert!(!ran.exists(), "{bad}: the CLI ran");
    }
}

#[test]
fn a_config_never_declares_mcp() {
    let cfg: Result<Config, _> = serde_json::from_str(r#"{"mcp":true}"#);
    assert!(cfg.unwrap_err().to_string().contains("mcp"));
    assert!(!Config::default().offers().capabilities.mcp);
}

#[test]
fn host_mode_declares_no_sandbox_refuses_skip_permissions_and_isolates_home() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new().unwrap();
    let real_home = dir.path().join("real-home");
    fs::create_dir_all(&real_home).unwrap();
    let cred_src = dir.path().join("host-oauth-token");
    fs::write(&cred_src, "{\"access_token\":\"host-tok-999\"}\n").unwrap();

    let observed_home = dir.path().join("observed-home");
    let observed_args = dir.path().join("observed-args");
    let observed_cred = dir.path().join("observed-cred");
    let observed_mode = dir.path().join("observed-mode");
    let observed_gh = dir.path().join("observed-gh");
    let observed_claude = dir.path().join("observed-claude");
    let fixture = write_executable(
        dir.path(),
        "ag-host",
        &format!(
            "#!/bin/sh\n\
             printf '%s' \"$HOME\" > \"{}\"\n\
             printf '%s\\n' \"$@\" > \"{}\"\n\
             cat \"$HOME/.gemini/antigravity-cli/antigravity-oauth-token\" > \"{}\"\n\
             stat -c '%a' \"$HOME/.gemini/antigravity-cli/antigravity-oauth-token\" > \"{}\"\n\
             printf '%s' \"${{GH_TOKEN_SOCKET:-}}\" > \"{}\"\n\
             printf '%s' \"${{CLAUDE_CODE_OAUTH_TOKEN:-}}${{ANTHROPIC_API_KEY:-}}\" > \"{}\"\n\
             printf '{{\"conversation_id\":\"host-sess-1\",\"type\":\"result\",\"result\":\"ok\"}}\\n'\n",
            observed_home.display(),
            observed_args.display(),
            observed_cred.display(),
            observed_mode.display(),
            observed_gh.display(),
            observed_claude.display(),
        ),
    );

    // Sandbox default keeps Sandbox::Needs.
    let sandbox_cfg = base_config(fixture.clone());
    assert_eq!(sandbox_cfg.offers().capabilities.sandbox, Sandbox::Needs);

    // host_mode = true refuses dangerously_skip_permissions = true.
    let mut invalid = base_config(fixture.clone());
    invalid.host_mode = true;
    invalid.dangerously_skip_permissions = true;
    let err = invalid.validate().unwrap_err();
    assert!(err.contains("dangerously_skip_permissions"), "{err}");

    // host_mode = true with safe flags declares Sandbox::None and isolates $HOME.
    let mut cfg = base_config(fixture);
    cfg.host_mode = true;
    cfg.dangerously_skip_permissions = false;
    cfg.mode = Some(Mode::Auto);
    cfg.credential_mode = crate::CredentialMode::HomeFile;
    cfg.credential_file = Some(cred_src);
    cfg.validate().unwrap();
    assert_eq!(cfg.offers().capabilities.sandbox, Sandbox::None);

    let inv = Invocation::from_config(
        &cfg,
        dir.path().to_path_buf(),
        "host task".to_owned(),
        None,
        None,
        None,
    );
    assert!(!inv.dangerously_skip_permissions);
    assert_eq!(inv.mode, Some(Mode::Auto));

    // Confirm describe reports Sandbox::None.
    let desc = describe(&cfg);
    match desc.answer {
        Answer::Describe { capabilities, .. } => {
            assert_eq!(capabilities.unwrap().sandbox, Sandbox::None);
        }
        other => panic!("expected Describe, got {other:?}"),
    }

    let turn = run_invocation(&cfg, &inv, &|| false);
    assert_eq!(turn.error, None, "{turn:?}");
    assert_eq!(turn.session.as_deref(), Some("host-sess-1"));

    let child_home = fs::read_to_string(&observed_home).unwrap();
    let current_home = std::env::var("HOME").unwrap_or_default();
    assert_ne!(child_home, current_home, "host turn reused real $HOME");
    assert!(
        !real_home.join(".gemini").exists(),
        "real $HOME must never be written"
    );
    assert_eq!(
        fs::read_to_string(&observed_cred).unwrap(),
        "{\"access_token\":\"host-tok-999\"}"
    );
    assert_eq!(fs::read_to_string(&observed_mode).unwrap().trim(), "600");
    let args = fs::read_to_string(&observed_args).unwrap();
    assert!(args.contains("--mode\nauto\n"), "{args}");
    assert!(!args.contains("--dangerously-skip-permissions"), "{args}");
    assert!(
        fs::read_to_string(&observed_claude).unwrap().is_empty(),
        "Anthropic credentials must remain stripped in host mode"
    );
    // Verify the isolated per-turn home directory was cleaned up after exit.
    assert!(
        !Path::new(&child_home).exists(),
        "isolated host HOME {child_home} should be cleaned up on turn exit"
    );
    let _ = fs::metadata(dir.path()).unwrap().permissions().mode();
}
