//! Executable fixture tests for the `burrow-engine-anthropic` `conary-plugin v5`
//! `job` binary across both `claude` (subscription CLI) and `claude-api` modes.

mod api;
mod common;

use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use burrow_engine_anthropic::plugin::{
    CONFIG_ENV, DiagnosticEvent, EXIT_FAILURE, EXIT_OK, EXIT_REFUSED_AUTH, EXIT_REFUSED_USAGE,
    RefusalKind,
};
use conary_core::plugin::{Answer, Choice, Context, Kind, Next, Plugin, Response, Sandbox, Task};
use rustix::process::{Pid, Signal, kill_process};
use serde_json::json;

use common::{
    TEST_MODEL_A, TEST_MODEL_B, default_limits, install_fake_claude_cli, invoke_raw,
    make_v5_job_request, make_wrapper, parse_diagnostics, plugin_bin, test_models, wait_until_gone,
    write_config,
};

#[test]
fn both_modes_pass_conarys_describe_and_conformance_check() {
    let dir = tempfile::tempdir().unwrap();
    for (mode, expected_engine, expected_meters) in [
        ("claude", "claude", vec!["five_hour", "seven_day"]),
        ("claude-api", "claude-api", vec!["credit"]),
    ] {
        let cfg = write_config(
            dir.path(),
            &format!("{mode}.json"),
            &json!({
                "mode": mode,
                "models": test_models(),
                "defaultModel": TEST_MODEL_A,
                "defaultEffort": "medium",
                "program": "/nonexistent/claude-must-not-run-on-conformance",
            }),
        );
        // No credentials provided: describe and conformance run strictly offline.
        let wrapper = make_wrapper(dir.path(), &format!("plugin-{mode}"), &cfg, &[]);
        let plugin = Plugin::describe(&wrapper, default_limits()).expect("describe");
        assert_eq!(plugin.identity.engine, expected_engine);
        assert_eq!(plugin.kinds(), [Kind::Job]);
        let offers = plugin.offers().expect("v5 offers");
        assert_eq!(offers.capabilities.sandbox, Sandbox::Needs);
        assert!(offers.capabilities.network);
        assert!(offers.capabilities.mcp);
        assert!(offers.capabilities.devices.is_empty());
        assert_eq!(offers.meters, expected_meters);
        assert_eq!(offers.models.len(), 2);
        assert_eq!(offers.models[0].id, TEST_MODEL_A);

        let checked = plugin.check().expect("cn plugin check conformance");
        assert_eq!(checked, [Kind::Job]);
    }
}

#[test]
fn claude_mode_fresh_continuation_model_effort_and_capability_grants() {
    let dir = tempfile::tempdir().unwrap();
    let workdir = dir.path().join("repo");
    let rec = dir.path().join("rec");
    fs::create_dir_all(&workdir).unwrap();
    fs::create_dir_all(&rec).unwrap();

    let fake_cli = dir.path().join("fake-claude");
    install_fake_claude_cli(&fake_cli, &rec);

    let cfg = write_config(
        dir.path(),
        "claude.json",
        &json!({
            "mode": "claude",
            "models": test_models(),
            "defaultModel": TEST_MODEL_A,
            "defaultEffort": "medium",
            "program": fake_cli,
            "credentialEnv": "CUSTOM_CLAUDE_LOGIN",
        }),
    );

    let caps_json = json!(["workspace", "probe", "mcp", "network"]).to_string();
    let mcp_json = json!({
        "conary": {
            "type": "stdio",
            "command": "/bin/socat",
            "args": ["-", "UNIX-CONNECT:/run/agent/mcp-0.sock"]
        }
    })
    .to_string();

    let wrapper = make_wrapper(
        dir.path(),
        "plugin-claude",
        &cfg,
        &[
            ("CUSTOM_CLAUDE_LOGIN", "oauth-token-fixture-value"),
            ("ANTHROPIC_API_KEY", "must-never-reach-claude-mode"),
            ("GH_TOKEN", "ghp_must_never_be_forwarded"),
            ("GITHUB_TOKEN", "ghs_must_never_be_forwarded"),
            ("GH_TOKEN_SOCKET", "/run/must-never-be-forwarded.sock"),
            ("PROJ_CAPABILITIES", &caps_json),
            ("PROJ_PROBE_SOCKET", "/run/agent/probe.sock"),
            ("PROJ_HOST_JOBS_SOCKET", "/run/agent/ungranted-jobs.sock"),
            ("HTTP_PROXY", "http://10.0.2.100:3128"),
            ("PROJ_MCP_SERVERS", &mcp_json),
        ],
    );

    let plugin = Plugin::describe(&wrapper, default_limits()).unwrap();

    // 1. Fresh job using runtime default model and effort.
    let fresh_task = Task::Job {
        repo: "owner/demo".into(),
        workdir: workdir.to_string_lossy().into_owned(),
        prompt: "fresh: summarize repo".into(),
        model: None,
        effort: None,
    };
    let fresh_resp = plugin.call(fresh_task).expect("fresh job succeeds");
    assert_eq!(fresh_resp.by.engine, "claude");
    assert_eq!(fresh_resp.by.session.as_deref(), Some("sess-fresh-01"));
    let exec = fresh_resp.execution.expect("provenance");
    assert_eq!(exec.model.as_deref(), Some(TEST_MODEL_A));
    assert_eq!(exec.effort.as_deref(), Some("medium"));
    assert_eq!(exec.context, Context::Fresh);
    match fresh_resp.answer {
        Answer::Job { summary, next } => {
            assert_eq!(
                summary,
                format!("answered model={TEST_MODEL_A} effort=medium")
            );
            assert_eq!(next, Next::Done);
        }
        other => panic!("unexpected answer: {other:?}"),
    }

    // Verify rendered CLI flags and environment isolation.
    let args = fs::read_to_string(rec.join("args")).unwrap();
    assert!(args.contains("--strict-mcp-config"), "{args}");
    assert!(args.contains("--mcp-config"), "{args}");
    assert!(args.contains("conary"), "{args}");
    assert!(
        args.contains(&format!("--add-dir\n{}", workdir.display())),
        "{args}"
    );
    assert!(!args.contains("oauth-token-fixture-value"), "{args}");
    assert!(!args.contains("ghp_must_never_be_forwarded"), "{args}");

    let child_env = fs::read_to_string(rec.join("env")).unwrap();
    assert!(
        child_env.contains("CLAUDE_CODE_OAUTH_TOKEN=oauth-token-fixture-value"),
        "{child_env}"
    );
    assert!(
        child_env.contains("PROJ_PROBE_SOCKET=/run/agent/probe.sock"),
        "{child_env}"
    );
    assert!(
        child_env.contains("HTTP_PROXY=http://10.0.2.100:3128"),
        "{child_env}"
    );
    assert!(
        child_env.contains("ENABLE_CLAUDEAI_MCP_SERVERS=false"),
        "{child_env}"
    );
    assert!(child_env.contains("DISABLE_AUTOUPDATER=1"), "{child_env}");
    for forbidden in [
        "ANTHROPIC_API_KEY=",
        "GH_TOKEN=",
        "GITHUB_TOKEN=",
        "GH_TOKEN_SOCKET=",
        "PROJ_HOST_JOBS_SOCKET=",
        "CUSTOM_CLAUDE_LOGIN=",
    ] {
        assert!(
            !child_env.contains(forbidden),
            "forbidden env {forbidden} leaked:\n{child_env}"
        );
    }

    // 2. Continuation resuming the returned session with an explicit runtime
    // model and effort choice.
    let cont_task = Task::Job {
        repo: "owner/demo".into(),
        workdir: workdir.to_string_lossy().into_owned(),
        prompt: "continue: follow up".into(),
        model: Some(TEST_MODEL_B.into()),
        effort: Some("high".into()),
    };
    let cont_resp = plugin
        .call_with(
            cont_task,
            Some(Choice {
                engine: "claude".into(),
                model: Some(TEST_MODEL_B.into()),
                effort: Some("high".into()),
            }),
            Context::Continue {
                session: "sess-fresh-01".into(),
            },
            None,
        )
        .expect("continuation succeeds");
    assert_eq!(cont_resp.by.session.as_deref(), Some("sess-fresh-01"));
    let cont_exec = cont_resp.execution.expect("provenance");
    assert_eq!(cont_exec.model.as_deref(), Some(TEST_MODEL_B));
    assert_eq!(cont_exec.effort.as_deref(), Some("high"));
    assert_eq!(
        cont_exec.context,
        Context::Continue {
            session: "sess-fresh-01".into()
        }
    );
    match cont_resp.answer {
        Answer::Job { summary, next } => {
            assert_eq!(
                summary,
                format!("continued sess-fresh-01 model={TEST_MODEL_B} effort=high")
            );
            assert_eq!(next, Next::Done);
        }
        other => panic!("unexpected answer: {other:?}"),
    }
    let cont_args = fs::read_to_string(rec.join("args")).unwrap();
    assert!(cont_args.contains("--resume\nsess-fresh-01"), "{cont_args}");
    assert!(
        cont_args.contains(&format!("--model\n{TEST_MODEL_B}")),
        "{cont_args}"
    );
    assert!(cont_args.contains("--effort\nhigh"), "{cont_args}");

    // 3. Unoffered model/effort is rejected before running.
    let bad_task = Task::Job {
        repo: "owner/demo".into(),
        workdir: workdir.to_string_lossy().into_owned(),
        prompt: "fresh: bad model".into(),
        model: Some("unoffered-model".into()),
        effort: None,
    };
    assert!(plugin.call(bad_task).is_err());
}

#[test]
fn claude_mode_refusals_failures_and_usage_diagnostics() {
    let dir = tempfile::tempdir().unwrap();
    let workdir = dir.path().join("repo");
    let rec = dir.path().join("rec");
    fs::create_dir_all(&workdir).unwrap();
    fs::create_dir_all(&rec).unwrap();

    let fake_cli = dir.path().join("fake-claude");
    install_fake_claude_cli(&fake_cli, &rec);

    let cfg_val = json!({
        "mode": "claude",
        "models": test_models(),
        "defaultModel": TEST_MODEL_A,
        "program": fake_cli,
    });
    let cfg_path = write_config(dir.path(), "claude.json", &cfg_val);
    let offers = burrow_engine_anthropic::plugin::PluginConfig::from_file(&cfg_path)
        .unwrap()
        .offers();

    // Success emits structured usage event on stderr alongside valid v5 Response on stdout.
    let req = make_v5_job_request(
        "claude",
        offers.clone(),
        &workdir,
        "fresh: check usage",
        None,
        Context::Fresh,
    );
    let out = invoke_raw(
        &cfg_path,
        &req,
        &[("CLAUDE_CODE_OAUTH_TOKEN", "oauth-token-fixture")],
    );
    assert_eq!(out.status.code(), Some(i32::from(EXIT_OK)));
    let resp: Response = serde_json::from_slice(&out.stdout).expect("valid stdout Response");
    assert_eq!(resp.by.session.as_deref(), Some("sess-fresh-01"));
    let diags = parse_diagnostics(&out.stderr);
    assert!(matches!(
        diags.as_slice(),
        [DiagnosticEvent::Usage { usage, .. }]
            if usage.input_tokens == 120
                && usage.output_tokens == 45
                && usage.total_cost_usd == Some(0.015)
    ));

    // Usage refusal: nonzero exit (EXIT_REFUSED_USAGE), empty stdout, normalized refusal on stderr.
    let req = make_v5_job_request(
        "claude",
        offers.clone(),
        &workdir,
        "usage-refusal",
        None,
        Context::Fresh,
    );
    let out = invoke_raw(
        &cfg_path,
        &req,
        &[("CLAUDE_CODE_OAUTH_TOKEN", "oauth-token-fixture")],
    );
    assert_eq!(out.status.code(), Some(i32::from(EXIT_REFUSED_USAGE)));
    assert!(
        out.stdout.is_empty(),
        "refusal must write nothing to stdout"
    );
    let diags = parse_diagnostics(&out.stderr);
    match diags.as_slice() {
        [DiagnosticEvent::Refusal { refusal, .. }] => {
            assert_eq!(refusal.kind, RefusalKind::UsageLimit);
            assert_eq!(refusal.resets_at, Some(1_793_822_400));
            assert_eq!(refusal.reason, "Claude reached its weekly allowance");
            assert!(!refusal.reason.contains("https://"));
        }
        other => panic!("expected usage refusal diagnostic, got {other:?}"),
    }

    // Login refusal from stream: EXIT_REFUSED_AUTH, empty stdout.
    let req = make_v5_job_request(
        "claude",
        offers.clone(),
        &workdir,
        "login-refusal",
        None,
        Context::Fresh,
    );
    let out = invoke_raw(
        &cfg_path,
        &req,
        &[("CLAUDE_CODE_OAUTH_TOKEN", "oauth-token-fixture")],
    );
    assert_eq!(out.status.code(), Some(i32::from(EXIT_REFUSED_AUTH)));
    assert!(out.stdout.is_empty());
    assert!(matches!(
        parse_diagnostics(&out.stderr).as_slice(),
        [DiagnosticEvent::Refusal { refusal, .. }]
            if refusal.kind == RefusalKind::LoginExpired && refusal.resets_at.is_none()
    ));

    // Missing login credential env: refused before invoking CLI.
    let req = make_v5_job_request(
        "claude",
        offers.clone(),
        &workdir,
        "fresh: no creds",
        None,
        Context::Fresh,
    );
    let out = invoke_raw(&cfg_path, &req, &[]);
    assert_eq!(out.status.code(), Some(i32::from(EXIT_REFUSED_AUTH)));
    assert!(out.stdout.is_empty());
    assert!(matches!(
        parse_diagnostics(&out.stderr).as_slice(),
        [DiagnosticEvent::Refusal { refusal, .. }] if refusal.kind == RefusalKind::LoginExpired
    ));

    // Ordinary failures (error result and nonzero crash): EXIT_FAILURE, empty stdout.
    for prompt in ["fail-error-result", "fail-crash"] {
        let req = make_v5_job_request(
            "claude",
            offers.clone(),
            &workdir,
            prompt,
            None,
            Context::Fresh,
        );
        let out = invoke_raw(
            &cfg_path,
            &req,
            &[("CLAUDE_CODE_OAUTH_TOKEN", "oauth-token-fixture")],
        );
        assert_eq!(out.status.code(), Some(i32::from(EXIT_FAILURE)), "{prompt}");
        assert!(out.stdout.is_empty(), "{prompt}");
        assert!(
            matches!(
                parse_diagnostics(&out.stderr).as_slice(),
                [DiagnosticEvent::Error { .. }]
            ),
            "{prompt}"
        );
    }
}

#[test]
fn cancellation_kills_entire_harness_process_group_with_no_survivors() {
    let dir = tempfile::tempdir().unwrap();
    let workdir = dir.path().join("repo");
    let rec = dir.path().join("rec");
    fs::create_dir_all(&workdir).unwrap();
    fs::create_dir_all(&rec).unwrap();

    let fake_cli = dir.path().join("fake-claude");
    install_fake_claude_cli(&fake_cli, &rec);

    let cfg_path = write_config(
        dir.path(),
        "claude-cancel.json",
        &json!({
            "mode": "claude",
            "models": test_models(),
            "defaultModel": TEST_MODEL_A,
            "program": fake_cli,
        }),
    );

    // Case A: Conary runner cancellation (`call_cancellable`, sends SIGKILL to plugin PGID).
    let wrapper = make_wrapper(
        dir.path(),
        "plugin-cancel",
        &cfg_path,
        &[("CLAUDE_CODE_OAUTH_TOKEN", "oauth-token-fixture")],
    );
    let plugin = Plugin::describe(&wrapper, default_limits()).unwrap();
    let cancel_marker = dir.path().join("cancelled");
    let pids_file = rec.join("pids");
    let cancel_bg = cancel_marker.clone();
    let pids_bg = pids_file.clone();
    let trigger = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !pids_bg.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(pids_bg.exists(), "fake claude never wrote pids");
        fs::write(&cancel_bg, "stop").unwrap();
    });
    let err = plugin
        .call_cancellable(
            Task::Job {
                repo: "owner/demo".into(),
                workdir: workdir.to_string_lossy().into_owned(),
                prompt: "hang-cancel".into(),
                model: None,
                effort: None,
            },
            Some(&cancel_marker),
        )
        .unwrap_err();
    trigger.join().unwrap();
    assert!(err.0.contains("cancelled"), "{err}");
    let pids: Vec<i32> = fs::read_to_string(&pids_file)
        .unwrap()
        .lines()
        .map(|l| l.trim().parse().unwrap())
        .collect();
    assert_eq!(pids.len(), 2);
    for pid in pids {
        assert!(
            wait_until_gone(pid, Duration::from_secs(3)),
            "process {pid} survived Conary cancellation"
        );
    }

    // Case B: Direct SIGTERM to the plugin process (outside Conary's PGID).
    let _ = fs::remove_file(&pids_file);
    let offers = burrow_engine_anthropic::plugin::PluginConfig::from_file(&cfg_path)
        .unwrap()
        .offers();
    let req = make_v5_job_request(
        "claude",
        offers.clone(),
        &workdir,
        "hang-cancel",
        None,
        Context::Fresh,
    );
    let mut cmd = Command::new(plugin_bin());
    cmd.env(CONFIG_ENV, &cfg_path)
        .env("CLAUDE_CODE_OAUTH_TOKEN", "oauth-token-fixture")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    {
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(&serde_json::to_vec(&req).unwrap()).unwrap();
        stdin.write_all(b"\n").unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while !pids_file.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        pids_file.exists(),
        "fake claude never wrote pids for SIGTERM"
    );
    let plugin_pid = Pid::from_child(&child);
    kill_process(plugin_pid, Signal::TERM).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let pids: Vec<i32> = fs::read_to_string(&pids_file)
        .unwrap()
        .lines()
        .map(|l| l.trim().parse().unwrap())
        .collect();
    assert_eq!(pids.len(), 2);
    for pid in pids {
        assert!(
            wait_until_gone(pid, Duration::from_secs(3)),
            "process {pid} survived SIGTERM cancellation"
        );
    }

    // Case C: Normal exit kills any lingering background grandchild left by the harness.
    let req = make_v5_job_request(
        "claude",
        offers,
        &workdir,
        "linger-exit",
        None,
        Context::Fresh,
    );
    let out = invoke_raw(
        &cfg_path,
        &req,
        &[("CLAUDE_CODE_OAUTH_TOKEN", "oauth-token-fixture")],
    );
    assert_eq!(out.status.code(), Some(i32::from(EXIT_OK)));
    let grand: i32 = fs::read_to_string(rec.join("grand"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(
        wait_until_gone(grand, Duration::from_secs(3)),
        "lingering grandchild {grand} survived normal exit"
    );
}
