use std::collections::BTreeMap;
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;

use conary_core::plugin::{
    Answer, Choice, Context, Execution, Kind, Model, Request, Sandbox, Task,
};
use serde_json::json;

use super::*;

fn sample_model(id: &str, efforts: &[&str]) -> Model {
    Model {
        id: id.to_owned(),
        efforts: efforts.iter().map(|e| (*e).to_owned()).collect(),
        context_tokens: Some(200_000),
        max_output_tokens: Some(16_384),
    }
}

fn sample_config(mode: Mode) -> PluginConfig {
    PluginConfig {
        mode,
        models: vec![
            sample_model("test-model-a", &["low", "medium", "high"]),
            sample_model("test-model-b", &["low", "high"]),
        ],
        default_model: Some("test-model-a".to_owned()),
        default_effort: Some("medium".to_owned()),
        ..PluginConfig::default()
    }
}

#[test]
fn describe_reports_runtime_config_and_needs_sandbox() {
    for (mode, expected_engine, expected_meters) in [
        (Mode::Claude, "claude", vec!["five_hour", "seven_day"]),
        (Mode::ClaudeApi, "claude-api", vec!["credit"]),
    ] {
        let cfg = sample_config(mode);
        let res = handle_request(&cfg, &Request::describe(), &|_| None, 0);
        assert_eq!(res.exit_code, EXIT_OK);
        let resp = res.response.expect("describe response");
        assert_eq!(resp.by.engine, expected_engine);
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
            panic!("expected describe");
        };
        assert!(versions.iter().any(|v| v == conary_core::plugin::CONTRACT));
        assert!(kinds.is_empty());
        assert_eq!(v2_kinds, [Kind::Job]);
        assert!(v4_kinds.is_empty());
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "test-model-a");
        let caps = capabilities.expect("capabilities");
        assert_eq!(caps.sandbox, Sandbox::Needs);
        assert!(caps.network);
        assert!(caps.mcp);
        assert!(caps.devices.is_empty());
        assert_eq!(meters, expected_meters);
    }
}

/// A host-mode `proj` wrapper runs on the host, where no sandbox is given:
/// it declares none, so proj's host launch (which refuses an engine that
/// needs one) runs it.
#[test]
fn describe_in_host_mode_declares_no_sandbox() {
    for mode in [Mode::Claude, Mode::ClaudeApi] {
        let cfg = PluginConfig {
            host_mode: true,
            ..sample_config(mode)
        };
        let res = handle_request(&cfg, &Request::describe(), &|_| None, 0);
        let Answer::Describe { capabilities, .. } = res.response.expect("describe").answer else {
            panic!("expected describe");
        };
        assert_eq!(capabilities.expect("capabilities").sandbox, Sandbox::None);
    }
}

#[test]
fn config_validation_rejects_bad_defaults_and_nonpositive_budget() {
    let mut cfg = sample_config(Mode::Claude);
    cfg.default_model = Some("missing-model".to_owned());
    assert!(cfg.validate().is_err());

    let mut cfg = sample_config(Mode::Claude);
    cfg.default_effort = Some("extreme".to_owned());
    assert!(cfg.validate().is_err());

    let mut cfg = sample_config(Mode::ClaudeApi);
    cfg.max_budget_usd = Some(0);
    assert!(cfg.validate().is_err());
}

#[test]
fn grants_from_env_allowlists_granted_capabilities_and_strips_github_and_creds() {
    let dir = tempfile::tempdir().unwrap();
    let env_map: BTreeMap<&str, String> = BTreeMap::from([
        ("HOME", "/home/dev".into()),
        ("PATH", "/usr/bin".into()),
        ("GH_TOKEN", "ghp_secret".into()),
        ("GITHUB_TOKEN", "ghs_secret".into()),
        ("GH_TOKEN_SOCKET", "/run/gh.sock".into()),
        ("CLAUDE_CODE_OAUTH_TOKEN", "oauth-secret".into()),
        ("ANTHROPIC_API_KEY", "api-secret".into()),
        ("UNRELATED_SECRET", "do-not-forward".into()),
        (
            "PROJ_CAPABILITIES",
            json!(["workspace", "probe", "mcp", "github", "environment"]).to_string(),
        ),
        ("PROJ_PROBE_SOCKET", "/run/agent/probe.sock".into()),
        ("PROJ_HOST_JOBS_SOCKET", "/run/agent/host-jobs.sock".into()),
        ("HTTP_PROXY", "http://10.0.2.100:3128".into()),
        (
            "PROJ_MCP_SERVERS",
            json!({
                "notes": {
                    "type": "stdio",
                    "command": "/bin/socat",
                    "args": ["-", "UNIX-CONNECT:/run/agent/mcp-0.sock"],
                    "env": {
                        "KEEP": "yes",
                        "GH_TOKEN": "must-be-stripped"
                    }
                }
            })
            .to_string(),
        ),
    ]);
    let grants = grants::from_env(&|k| env_map.get(k).cloned(), dir.path()).unwrap();
    assert!(!grants.inherit_environment);
    assert_eq!(grants.paths, vec![(dir.path().to_path_buf(), true)]);
    assert_eq!(
        grants.env.get("HOME").map(String::as_str),
        Some("/home/dev")
    );
    assert_eq!(
        grants.env.get("PROJ_PROBE_SOCKET").map(String::as_str),
        Some("/run/agent/probe.sock")
    );
    assert_eq!(
        grants
            .env
            .get("ENABLE_CLAUDEAI_MCP_SERVERS")
            .map(String::as_str),
        Some("false")
    );
    assert_eq!(
        grants.env.get("DISABLE_AUTOUPDATER").map(String::as_str),
        Some("1")
    );
    // Ungranted capabilities and forbidden variables are absent.
    for absent in [
        "PROJ_HOST_JOBS_SOCKET",
        "HTTP_PROXY",
        "GH_TOKEN",
        "GITHUB_TOKEN",
        "GH_TOKEN_SOCKET",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "ANTHROPIC_API_KEY",
        "UNRELATED_SECRET",
    ] {
        assert!(!grants.env.contains_key(absent), "{absent} leaked");
    }
    let notes = grants.servers.get("notes").expect("mcp server");
    assert_eq!(notes.command, "/bin/socat");
    assert_eq!(notes.env.get("KEEP").map(String::as_str), Some("yes"));
    assert!(!notes.env.contains_key("GH_TOKEN"));
    // `github` and `environment` are never retained in PROJ_CAPABILITIES.
    let caps: Vec<String> = serde_json::from_str(&grants.env["PROJ_CAPABILITIES"]).unwrap();
    assert!(!caps.contains(&"github".to_owned()));
    assert!(!caps.contains(&"environment".to_owned()));
}

#[test]
fn grants_from_env_withholds_mcp_and_workspace_when_ungranted() {
    let dir = tempfile::tempdir().unwrap();
    let env_map: BTreeMap<&str, String> = BTreeMap::from([
        ("PROJ_CAPABILITIES", "[]".into()),
        (
            "PROJ_MCP_SERVERS",
            json!({"notes": {"command": "/bin/socat", "args": []}}).to_string(),
        ),
    ]);
    let grants = grants::from_env(&|k| env_map.get(k).cloned(), dir.path()).unwrap();
    assert!(grants.paths.is_empty());
    assert!(grants.servers.is_empty());
}

#[test]
fn resolve_choice_respects_requested_and_defaults() {
    let cfg = sample_config(Mode::Claude);
    let offers = cfg.offers();
    assert_eq!(
        resolve_choice(&cfg, &offers, None, None, None).unwrap(),
        (Some("test-model-a".into()), Some("medium".into()))
    );
    let exec = Execution {
        choice: Some(Choice {
            engine: "claude".into(),
            model: Some("test-model-b".into()),
            effort: Some("high".into()),
        }),
        context: Context::Fresh,
    };
    assert_eq!(
        resolve_choice(&cfg, &offers, None, None, Some(&exec)).unwrap(),
        (Some("test-model-b".into()), Some("high".into()))
    );
    // Effort without model fails.
    assert!(resolve_choice(&cfg, &offers, None, Some("high"), None).is_err());
    // Unoffered model fails.
    assert!(resolve_choice(&cfg, &offers, Some("unknown-model"), None, None).is_err());
    // Disagreeing task and execution choices fail.
    assert!(resolve_choice(&cfg, &offers, Some("test-model-a"), None, Some(&exec)).is_err());
}

#[test]
fn extract_usage_reads_result_and_falls_back_to_assistant() {
    let records = vec![
        json!({
            "type": "assistant",
            "message": {
                "id": "msg_1",
                "usage": {
                    "input_tokens": 100,
                    "output_tokens": 5,
                    "cache_read_input_tokens": 20,
                    "cache_creation_input_tokens": 10
                }
            }
        }),
        json!({
            "type": "result",
            "is_error": false,
            "result": "done",
            "total_cost_usd": 0.0125,
            "usage": {
                "input_tokens": 120,
                "output_tokens": 45,
                "cache_read_input_tokens": 20,
                "cache_creation_input_tokens": 10
            }
        }),
    ];
    let usage = run::extract_usage(&records);
    assert_eq!(usage.input_tokens, 120);
    assert_eq!(usage.output_tokens, 45);
    assert_eq!(usage.cache_read_input_tokens, 20);
    assert_eq!(usage.cache_creation_input_tokens, 10);
    assert_eq!(usage.total_cost_usd, Some(0.0125));

    // Crash with no result usage falls back to assistant input/cache tokens.
    let partial = run::extract_usage(&records[..1]);
    assert_eq!(partial.input_tokens, 100);
    assert_eq!(partial.output_tokens, 0);
    assert_eq!(partial.cache_read_input_tokens, 20);
    assert_eq!(partial.cache_creation_input_tokens, 10);
    assert_eq!(partial.total_cost_usd, None);
}

#[test]
fn classify_output_maps_refusals_and_failures() {
    let ok_status = ExitStatus::from_raw(0);
    let fail_status = ExitStatus::from_raw(1 << 8);

    // Claude usage refusal with timestamp.
    let usage_stream = concat!(
        r#"{"type":"assistant","session_id":"s1","error":"rate_limit","api_error":"usage_limit_reached","api_error_params":{"rate_limit_info":{"status":"rejected","resetsAt":1793822400,"rateLimitType":"seven_day"}}}"#,
        "\n",
        r#"{"type":"result","subtype":"success","is_error":true,"api_error":"usage_limit_reached","result":"You've hit your weekly limit","session_id":"s1"}"#,
        "\n"
    );
    let out = run::classify_output(
        Mode::Claude,
        &Context::Fresh,
        usage_stream.as_bytes(),
        b"",
        ok_status,
        1_793_587_665,
    );
    match out {
        JobOutcome::Refused { refusal, .. } => {
            assert_eq!(refusal.kind, RefusalKind::UsageLimit);
            assert_eq!(refusal.resets_at, Some(1_793_822_400));
            assert_eq!(refusal.reason, "Claude reached its weekly allowance");
        }
        other => panic!("expected usage refusal, got {other:?}"),
    }

    // Claude login refusal.
    let login_stream = concat!(
        r#"{"type":"assistant","session_id":"s2","error":"authentication_failed","message":{"content":[{"type":"text","text":"Failed to authenticate: OAuth session expired"}]}}"#,
        "\n",
        r#"{"type":"result","is_error":true,"result":"Failed to authenticate: OAuth session expired","session_id":"s2"}"#,
        "\n"
    );
    let out = run::classify_output(
        Mode::Claude,
        &Context::Fresh,
        login_stream.as_bytes(),
        b"",
        ok_status,
        0,
    );
    match out {
        JobOutcome::Refused { refusal, .. } => {
            assert_eq!(refusal.kind, RefusalKind::LoginExpired);
            assert_eq!(refusal.resets_at, None);
        }
        other => panic!("expected login refusal, got {other:?}"),
    }

    // API budget refusal.
    let budget_stream = r#"{"type":"result","subtype":"error_max_budget_usd","is_error":true,"session_id":"s3","total_cost_usd":1.5}"#;
    let out = run::classify_output(
        Mode::ClaudeApi,
        &Context::Fresh,
        budget_stream.as_bytes(),
        b"",
        ok_status,
        0,
    );
    match out {
        JobOutcome::Refused { refusal, .. } => {
            assert_eq!(refusal.kind, RefusalKind::BudgetExhausted);
        }
        other => panic!("expected budget refusal, got {other:?}"),
    }

    // Ordinary failure without terminal result.
    let out = run::classify_output(
        Mode::Claude,
        &Context::Fresh,
        b"",
        b"boom on stderr",
        fail_status,
        0,
    );
    match out {
        JobOutcome::Failed { error, .. } => {
            assert!(error.contains("boom on stderr"), "{error}");
        }
        other => panic!("expected failure, got {other:?}"),
    }
}

#[test]
fn non_job_tasks_are_rejected() {
    let cfg = sample_config(Mode::Claude);
    let req = Request::work(
        conary_core::plugin::CONTRACT,
        Task::Review {
            repo: "owner/repo".into(),
            pr: 1,
            base: "1".repeat(40),
            head: "2".repeat(40),
            author: Identity {
                engine: "other".into(),
                session: Some("s".into()),
            },
        },
    );
    let res = handle_request(&cfg, &req, &|_| None, 0);
    assert_eq!(res.exit_code, EXIT_FAILURE);
    assert!(res.response.is_none());
}
