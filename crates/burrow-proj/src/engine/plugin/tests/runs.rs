//! Where a turn's plugin may run, and what a run must answer.

use std::fs;

use conary_core::plugin::Sandbox;
use tempfile::TempDir;

use super::super::launch::placed;
use super::*;

/// A plugin that records any invocation (including `describe`).
fn recording_plugin(tmp: &TempDir, cfg: &mut Config, engine: &str, sandbox: Sandbox) -> PathBuf {
    let desc = tmp.path().join(format!("{engine}-desc.json"));
    fs::write(&desc, describe_json_with(engine, vec![], vec![], sandbox)).unwrap();
    let ran = tmp.path().join(format!("{engine}-ran"));
    let plugin = tmp.path().join(format!("{engine}-plugin"));
    burrow_test::executable(
        &plugin,
        format!(
            "#!/bin/sh\ntouch {}\nreq=\"$(cat)\"\nif printf '%s' \"$req\" | grep -q '\"kind\":\"describe\"'; then cat {}; exit 0; fi\nexit 1\n",
            ran.display(),
            desc.display()
        ),
    );
    cfg.plugins.insert(engine.into(), plugin);
    ran
}

/// A sandbox-only plugin (wired from `burrow.agents.engines`) is refused on
/// a host turn before its executable ever runs, even for `describe`.
#[test]
fn host_turn_on_a_sandbox_only_plugin_is_refused_without_executing_it() {
    let tmp = TempDir::new().unwrap();
    let (mut cfg, project) = setup(&tmp);
    let ran = recording_plugin(&tmp, &mut cfg, "vm-only", Sandbox::Needs);
    cfg.sandbox_plugins.insert("vm-only".into(), vec![]);
    let job = make_job(&cfg, &project, "job-host-vm-only");
    let waitdir = job.turn_file("1", TurnFile::Waits);
    fs::create_dir_all(&waitdir).unwrap();
    let prompt = job.prompt("1");
    fs::write(&prompt, "Run on the host\n").unwrap();
    let choice = Choice {
        engine: "vm-only".into(),
        model: None,
        effort: None,
    };
    let refused = match spawn(&cfg, &job, "1", &prompt, &waitdir, None, true, "", &choice) {
        Err(SpawnError::Refused(r)) => r,
        Err(SpawnError::Failed(e)) => panic!("refused, not failed: {e}"),
        Ok(_) => panic!("sandbox-only plugin started on the host"),
    };
    assert_eq!(refused.kind, RefusalKind::Misplaced);
    assert!(!refused.kind.holds_the_engine());
    assert!(
        !ran.exists(),
        "sandbox-only plugin must not execute on the host, even for describe"
    );
}

/// A sandbox has the login and hosts of the engine it was made for alone: a
/// turn routed to another engine is refused before the sandbox is reached,
/// and holds nothing.
#[test]
fn a_sandboxed_turn_on_another_engine_than_its_sandboxs_is_refused_before_it_runs() {
    let tmp = TempDir::new().unwrap();
    let (mut cfg, project) = setup(&tmp);
    let (_, calls) = stand_in_sandbox(&tmp, &mut cfg);
    let ran = recording_plugin(&tmp, &mut cfg, "vm-engine", Sandbox::Needs);
    let job = make_job(&cfg, &project, "job-other-vm");
    crate::job::Request {
        project: project.to_string_lossy().into_owned(),
        agent: Some("demo-4".into()),
        agent_engine: Some("claude".into()),
        ..Default::default()
    }
    .save(&job)
    .unwrap();
    let waitdir = job.turn_file("1", TurnFile::Waits);
    fs::create_dir_all(&waitdir).unwrap();
    let prompt = job.prompt("1");
    fs::write(&prompt, "Work\n").unwrap();
    let choice = Choice {
        engine: "vm-engine".into(),
        model: None,
        effort: None,
    };
    let refused = match spawn(
        &cfg,
        &job,
        "1",
        &prompt,
        &waitdir,
        Some("demo-4"),
        true,
        "",
        &choice,
    ) {
        Err(SpawnError::Refused(r)) => r,
        Err(SpawnError::Failed(e)) => panic!("refused, not failed: {e}"),
        Ok(_) => panic!("ran in a sandbox made for another engine"),
    };
    assert_eq!(refused.kind, RefusalKind::Misplaced);
    assert!(refused.reason.contains("(claude)"), "{}", refused.reason);
    assert!(!refused.kind.holds_the_engine());
    assert!(!ran.exists(), "the plugin's job ran");
    assert!(!calls.exists(), "the sandbox was reached");
}

/// Every place a turn can be, and what may run there.
#[test]
fn an_engine_runs_only_where_the_turn_can_give_it_what_it_needs() {
    let described = |sandbox| {
        parse_describe(
            "e",
            describe_json_with("e", vec![], vec![], sandbox).as_bytes(),
        )
        .unwrap()
    };
    let needs = described(Sandbox::Needs);
    let plain = described(Sandbox::None);
    let kind = |r: Result<(), PluginRefusal>| r.err().map(|r| r.kind);
    // On the host: only an engine that needs a sandbox is refused.
    assert_eq!(
        kind(placed(None, None, "e", false, None, &needs)),
        Some(RefusalKind::Misplaced)
    );
    assert_eq!(kind(placed(None, None, "e", false, None, &plain)), None);
    assert_eq!(kind(placed(None, None, "e", true, None, &plain)), None);
    // In the sandbox made for it, or one from before that was kept.
    assert_eq!(
        kind(placed(Some("vm"), Some("e"), "e", false, None, &needs)),
        None
    );
    assert_eq!(
        kind(placed(Some("vm"), None, "e", false, None, &needs)),
        None
    );
    // In a sandbox made for another engine, and on the credit in any.
    assert_eq!(
        kind(placed(Some("vm"), Some("other"), "e", false, None, &needs)),
        Some(RefusalKind::Misplaced)
    );
    assert_eq!(
        kind(placed(Some("vm"), Some("e"), "e", true, None, &plain)),
        Some(RefusalKind::CreditExhausted)
    );
}

/// A sandbox plugin wired from `burrow.agents.engines` must have owner
/// approval in `burrow.agents.engines.NAME.capabilities` for everything a
/// `proj --vm` job VM exposes (e.g. `workspace`) as well as any extra
/// capability its `describe` declares (e.g. `kvm`).
#[test]
#[expect(clippy::too_many_lines, reason = "legacy test")]
fn sandboxed_turn_enforces_owner_approved_plugin_grants() {
    let all_vm_caps: Vec<String> = super::super::launch::VM_EXPOSED_CAPABILITIES
        .iter()
        .map(|c| c.name().to_owned())
        .collect();
    let without_workspace: Vec<String> = all_vm_caps
        .iter()
        .filter(|c| c.as_str() != "workspace")
        .cloned()
        .collect();

    // Case 1: owner omitted `workspace` -> refused BEFORE executing the plugin
    // at all (even for `describe`) and before touching `agent`.
    let tmp = TempDir::new().unwrap();
    let (mut cfg, project) = setup(&tmp);
    let (_, calls) = stand_in_sandbox(&tmp, &mut cfg);
    let ran = recording_plugin(&tmp, &mut cfg, "vm-engine", Sandbox::Needs);
    cfg.sandbox_plugins
        .insert("vm-engine".into(), without_workspace);
    let job = make_job(&cfg, &project, "job-no-ws");
    let waitdir = job.turn_file("1", TurnFile::Waits);
    fs::create_dir_all(&waitdir).unwrap();
    let prompt = job.prompt("1");
    fs::write(&prompt, "Work\n").unwrap();
    let choice = Choice {
        engine: "vm-engine".into(),
        model: None,
        effort: None,
    };
    let refused = match spawn(
        &cfg,
        &job,
        "1",
        &prompt,
        &waitdir,
        Some("demo-4"),
        true,
        "",
        &choice,
    ) {
        Err(SpawnError::Refused(r)) => r,
        Err(SpawnError::Failed(e)) => panic!("refused, not failed: {e}"),
        Ok(_) => panic!("launched sandbox plugin despite withheld workspace grant"),
    };
    assert_eq!(refused.kind, RefusalKind::Misplaced);
    assert!(refused.reason.contains("workspace"), "{}", refused.reason);
    assert!(!ran.exists(), "plugin executed despite withheld grant");
    assert!(
        !calls.exists(),
        "sandbox was reached despite withheld grant"
    );

    // Case 2: owner approved all job-VM capabilities, but `describe` declares
    // `devices: ["kvm"]` without owner approval -> refused via `burrow-grants`
    // before `Task::Job` runs.
    let tmp2 = TempDir::new().unwrap();
    let (mut cfg2, project2) = setup(&tmp2);
    let _ = stand_in_sandbox(&tmp2, &mut cfg2);
    let desc_kvm = serde_json::to_string(&Response {
        contract: CONTRACT_V1.to_owned(),
        by: conary_core::plugin::Identity {
            engine: "kvm-engine".to_owned(),
            session: None,
        },
        execution: None,
        answer: Answer::Describe {
            versions: vec![(*CONTRACTS.last().unwrap()).to_owned()],
            kinds: vec![],
            v2_kinds: vec![Kind::Job],
            v4_kinds: vec![],
            models: vec![],
            capabilities: Some(conary_core::plugin::Capabilities {
                network: true,
                mcp: false,
                devices: vec!["kvm".to_owned()],
                sandbox: Sandbox::Needs,
            }),
            meters: vec![],
        },
    })
    .unwrap();
    let desc_path = tmp2.path().join("desc-kvm.json");
    fs::write(&desc_path, desc_kvm).unwrap();
    let job_ran = tmp2.path().join("kvm-job-ran");
    let plugin = tmp2.path().join("kvm-plugin");
    burrow_test::executable(
        &plugin,
        format!(
            "#!/bin/sh\nreq=\"$(cat)\"\nif printf '%s' \"$req\" | grep -q '\"kind\":\"describe\"'; then cat {}; exit 0; fi\ntouch {}\nexit 1\n",
            desc_path.display(),
            job_ran.display()
        ),
    );
    cfg2.plugins.insert("kvm-engine".into(), plugin);
    cfg2.sandbox_plugins
        .insert("kvm-engine".into(), all_vm_caps.clone());
    let job2 = make_job(&cfg2, &project2, "job-unapproved-kvm");
    let waitdir2 = job2.turn_file("1", TurnFile::Waits);
    fs::create_dir_all(&waitdir2).unwrap();
    let prompt2 = job2.prompt("1");
    fs::write(&prompt2, "Work\n").unwrap();
    let choice2 = Choice {
        engine: "kvm-engine".into(),
        model: None,
        effort: None,
    };
    let refused2 = match spawn(
        &cfg2,
        &job2,
        "1",
        &prompt2,
        &waitdir2,
        Some("demo-4"),
        true,
        "",
        &choice2,
    ) {
        Err(SpawnError::Refused(r)) => r,
        Err(SpawnError::Failed(e)) => panic!("refused, not failed: {e}"),
        Ok(_) => panic!("launched sandbox plugin with unapproved kvm declaration"),
    };
    assert_eq!(refused2.kind, RefusalKind::Misplaced);
    assert!(refused2.reason.contains("kvm"), "{}", refused2.reason);
    assert!(!job_ran.exists(), "Task::Job ran despite unapproved kvm");

    // Case 2b: owner approved all job-VM capabilities (including `network`),
    // but `describe` omits `network` (`network: false`) -> refused via
    // `burrow-grants` (`Why::NotDeclared`) before `Task::Job` runs, because
    // the engine VM exposes the proxy for approved egress.
    let tmp2b = TempDir::new().unwrap();
    let (mut cfg2b, project2b) = setup(&tmp2b);
    let _ = stand_in_sandbox(&tmp2b, &mut cfg2b);
    let desc_no_net = serde_json::to_string(&Response {
        contract: CONTRACT_V1.to_owned(),
        by: conary_core::plugin::Identity {
            engine: "offline-engine".to_owned(),
            session: None,
        },
        execution: None,
        answer: Answer::Describe {
            versions: vec![(*CONTRACTS.last().unwrap()).to_owned()],
            kinds: vec![],
            v2_kinds: vec![Kind::Job],
            v4_kinds: vec![],
            models: vec![],
            capabilities: Some(conary_core::plugin::Capabilities {
                network: false,
                mcp: false,
                devices: vec![],
                sandbox: Sandbox::Needs,
            }),
            meters: vec![],
        },
    })
    .unwrap();
    let desc_no_net_path = tmp2b.path().join("desc-no-net.json");
    fs::write(&desc_no_net_path, desc_no_net).unwrap();
    let offline_job_ran = tmp2b.path().join("offline-job-ran");
    let offline_plugin = tmp2b.path().join("offline-plugin");
    burrow_test::executable(
        &offline_plugin,
        format!(
            "#!/bin/sh\nreq=\"$(cat)\"\nif printf '%s' \"$req\" | grep -q '\"kind\":\"describe\"'; then cat {}; exit 0; fi\ntouch {}\nexit 1\n",
            desc_no_net_path.display(),
            offline_job_ran.display()
        ),
    );
    cfg2b
        .plugins
        .insert("offline-engine".into(), offline_plugin);
    cfg2b
        .sandbox_plugins
        .insert("offline-engine".into(), all_vm_caps.clone());
    let job2b = make_job(&cfg2b, &project2b, "job-undeclared-network");
    let waitdir2b = job2b.turn_file("1", TurnFile::Waits);
    fs::create_dir_all(&waitdir2b).unwrap();
    let prompt2b = job2b.prompt("1");
    fs::write(&prompt2b, "Work\n").unwrap();
    let choice2b = Choice {
        engine: "offline-engine".into(),
        model: None,
        effort: None,
    };
    let refused2b = match spawn(
        &cfg2b,
        &job2b,
        "1",
        &prompt2b,
        &waitdir2b,
        Some("demo-4"),
        true,
        "",
        &choice2b,
    ) {
        Err(SpawnError::Refused(r)) => r,
        Err(SpawnError::Failed(e)) => panic!("refused, not failed: {e}"),
        Ok(_) => panic!("launched sandbox plugin despite undeclared VM network capability"),
    };
    assert_eq!(refused2b.kind, RefusalKind::Misplaced);
    assert!(refused2b.reason.contains("network"), "{}", refused2b.reason);
    assert!(
        !offline_job_ran.exists(),
        "Task::Job ran despite undeclared network capability"
    );

    // Case 3: all job-VM capabilities approved and matching `describe` -> allowed.
    let described_ok = parse_describe(
        "ok-engine",
        describe_json_with("ok-engine", vec![], vec![], Sandbox::Needs).as_bytes(),
    )
    .unwrap();
    assert!(
        placed(
            Some("vm"),
            Some("ok-engine"),
            "ok-engine",
            false,
            Some(&all_vm_caps),
            &described_ok
        )
        .is_ok()
    );
}

/// Whether a run is on the credit is the machine's one decision
/// (`Config::uses_credit_meter`), the same as its route and admission read:
/// `claude-api` is admitted through the credit's ledger whatever its
/// `describe` lists, and a plugin naming a `credit` meter is an ordinary run.
#[test]
fn only_the_credits_engine_runs_on_the_credit_whatever_a_plugin_declares() {
    for (engine, meters, on_credit) in [
        ("claude-api", vec![], true),
        ("custom", vec!["credit".to_owned()], false),
    ] {
        let tmp = TempDir::new().unwrap();
        let (mut cfg, project) = setup(&tmp);
        assert_eq!(cfg.uses_credit_meter(engine), on_credit);
        let desc = tmp.path().join("desc.json");
        fs::write(&desc, describe_json(engine, vec![], meters)).unwrap();
        let ledger = tmp.path().join("ledger");
        let plugin = tmp.path().join("plugin");
        burrow_test::executable(
            &plugin,
            format!(
                "#!/bin/sh\nreq=\"$(cat)\"\nif printf '%s' \"$req\" | grep -q '\"kind\":\"describe\"'; then cat {}; exit 0; fi\nprintf '%s' \"${{PROJ_CREDIT_LEDGER_FILE:-none}}\" > {}\nexit 1\n",
                desc.display(),
                ledger.display()
            ),
        );
        cfg.plugins.insert(engine.into(), plugin);
        let job = make_job(&cfg, &project, "job-credit");
        let waitdir = job.turn_file("1", TurnFile::Waits);
        fs::create_dir_all(&waitdir).unwrap();
        let prompt = job.prompt("1");
        fs::write(&prompt, "Spend\n").unwrap();
        let choice = Choice {
            engine: engine.into(),
            model: None,
            effort: None,
        };
        let mut running =
            spawn(&cfg, &job, "1", &prompt, &waitdir, None, true, "", &choice).unwrap();
        let status = running.child.take().unwrap().wait().unwrap();
        let _ = running.finish(status, false, &cfg, &job, "1", 0);
        let given = fs::read_to_string(&ledger).unwrap();
        assert_eq!(given != "none", on_credit, "{engine}: {given}");
        // An ordinary run is marked started as it starts; a credit's run
        // marks itself once the ledger admits it, which this one never did.
        let stream = fs::read_to_string(job.turn_file("1", TurnFile::Jsonl)).unwrap();
        assert_eq!(
            stream.contains("burrow.run"),
            !on_credit,
            "{engine}: {stream}"
        );
    }
}

/// A run that exits 0 without a response proved none of what the protocol
/// asks, whatever it streamed, and fails; one the turn cut after its answer
/// keeps that answer.
#[test]
fn a_run_without_a_response_fails_unless_it_was_cut_after_its_answer() {
    for cut in [false, true] {
        let tmp = TempDir::new().unwrap();
        let (mut cfg, project) = setup(&tmp);
        let desc = tmp.path().join("desc.json");
        fs::write(&desc, describe_json("mute", vec![], vec![])).unwrap();
        let said = tmp.path().join("said.jsonl");
        fs::write(
            &said,
            format!(
                "{}\n",
                serde_json::json!({"event": "stream",
                    "line": r#"{"type":"result","is_error":false,"result":"streamed only"}"#})
            ),
        )
        .unwrap();
        let plugin = tmp.path().join("mute-plugin");
        burrow_test::executable(
            &plugin,
            format!(
                "#!/bin/sh\nreq=\"$(cat)\"\nif printf '%s' \"$req\" | grep -q '\"kind\":\"describe\"'; then cat {}; exit 0; fi\ncat {} >&2\nexit 0\n",
                desc.display(),
                said.display()
            ),
        );
        cfg.plugins.insert("mute".into(), plugin);
        let job = make_job(&cfg, &project, "job-mute");
        let waitdir = job.turn_file("1", TurnFile::Waits);
        fs::create_dir_all(&waitdir).unwrap();
        let prompt = job.prompt("1");
        fs::write(&prompt, "Answer\n").unwrap();
        let choice = Choice {
            engine: "mute".into(),
            model: None,
            effort: None,
        };
        let mut running =
            spawn(&cfg, &job, "1", &prompt, &waitdir, None, true, "", &choice).unwrap();
        let status = running.child.take().unwrap().wait().unwrap();
        assert!(status.success());
        let res = running.finish(status, cut, &cfg, &job, "1", 0);
        if cut {
            assert_eq!(res.last.unwrap()["result"], "streamed only");
        } else {
            assert_eq!(res.rc, 1);
            assert!(
                res.last.is_none(),
                "an unverified run's stream was its answer"
            );
        }
    }
}

/// A run that fails (exits non-zero, with no response and no refusal) never
/// answers with a result it streamed, which nothing verified: only a streamed
/// error stays (the turn ends as one), and a cut run keeps its answer.
#[test]
fn a_failed_run_answers_only_with_an_error_unless_it_was_cut() {
    for (streamed, cut, kept) in [
        (
            r#"{"type":"result","is_error":false,"result":"looks done"}"#,
            false,
            false,
        ),
        (
            r#"{"type":"result","is_error":true,"result":"broke"}"#,
            false,
            true,
        ),
        (
            r#"{"type":"result","is_error":false,"result":"looks done"}"#,
            true,
            true,
        ),
    ] {
        let tmp = TempDir::new().unwrap();
        let (mut cfg, project) = setup(&tmp);
        let desc = tmp.path().join("desc.json");
        fs::write(&desc, describe_json("crash", vec![], vec![])).unwrap();
        let said = tmp.path().join("said.jsonl");
        fs::write(
            &said,
            format!(
                "{}\n",
                serde_json::json!({"event": "stream", "line": streamed})
            ),
        )
        .unwrap();
        let plugin = tmp.path().join("crash-plugin");
        burrow_test::executable(
            &plugin,
            format!(
                "#!/bin/sh\nreq=\"$(cat)\"\nif printf '%s' \"$req\" | grep -q '\"kind\":\"describe\"'; then cat {}; exit 0; fi\ncat {} >&2\nexit 3\n",
                desc.display(),
                said.display()
            ),
        );
        cfg.plugins.insert("crash".into(), plugin);
        let job = make_job(&cfg, &project, "job-crash");
        let waitdir = job.turn_file("1", TurnFile::Waits);
        fs::create_dir_all(&waitdir).unwrap();
        let prompt = job.prompt("1");
        fs::write(&prompt, "Answer\n").unwrap();
        let choice = Choice {
            engine: "crash".into(),
            model: None,
            effort: None,
        };
        let mut running =
            spawn(&cfg, &job, "1", &prompt, &waitdir, None, true, "", &choice).unwrap();
        let status = running.child.take().unwrap().wait().unwrap();
        assert!(!status.success());
        let res = running.finish(status, cut, &cfg, &job, "1", 0);
        assert_eq!(res.rc, 3);
        assert_eq!(res.last.is_some(), kept, "{streamed} cut={cut}");
    }
}

/// What a refused or failed run reports spending reaches its turn's usage
/// record, without the run becoming an answer: on a record of its own when it
/// streamed no result, in the result it streamed when that carries none.
#[test]
fn a_refused_or_failed_runs_usage_is_kept_for_its_turns_accounting() {
    let refusal = r#"{"event":"refusal","refusal":{"kind":"usage_limit","reason":"out"},"usage":{"input_tokens":70,"output_tokens":5,"total_cost_usd":0.4}}"#;
    let error = r#"{"event":"error","error":"broke","usage":{"input_tokens":70,"output_tokens":5,"total_cost_usd":0.4}}"#;
    let streamed = serde_json::json!({"event": "stream",
        "line": r#"{"type":"result","is_error":true,"result":"broke"}"#})
    .to_string();
    for (said, answered) in [
        (refusal.to_owned(), false),
        (format!("{streamed}\n{error}"), true),
    ] {
        let tmp = TempDir::new().unwrap();
        let (mut cfg, project) = setup(&tmp);
        let desc = tmp.path().join("desc.json");
        fs::write(&desc, describe_json("spender", vec![], vec![])).unwrap();
        let said_path = tmp.path().join("said.jsonl");
        fs::write(&said_path, format!("{said}\n")).unwrap();
        let plugin = tmp.path().join("spender-plugin");
        burrow_test::executable(
            &plugin,
            format!(
                "#!/bin/sh\nreq=\"$(cat)\"\nif printf '%s' \"$req\" | grep -q '\"kind\":\"describe\"'; then cat {}; exit 0; fi\ncat {} >&2\nexit 3\n",
                desc.display(),
                said_path.display()
            ),
        );
        cfg.plugins.insert("spender".into(), plugin);
        let job = make_job(&cfg, &project, "job-spender");
        let waitdir = job.turn_file("1", TurnFile::Waits);
        fs::create_dir_all(&waitdir).unwrap();
        let prompt = job.prompt("1");
        fs::write(&prompt, "Spend\n").unwrap();
        let choice = Choice {
            engine: "spender".into(),
            model: None,
            effort: None,
        };
        let mut running =
            spawn(&cfg, &job, "1", &prompt, &waitdir, None, true, "", &choice).unwrap();
        let status = running.child.take().unwrap().wait().unwrap();
        let res = running.finish(status, false, &cfg, &job, "1", 0);
        assert_eq!(res.last.is_some(), answered, "{said}");
        if let Some(last) = &res.last {
            assert!(stream::is_error(last), "{last}");
        }
        let record = burrow_probe::usage_record::TurnUsage::from_job_turn(
            &job.dir, job.id, 1, "spender", None, 0,
        )
        .unwrap();
        let run = record.runs.last().expect("the run is recorded");
        assert_eq!((run.input_tokens, run.output_tokens), (70, 5), "{said}");
        assert_eq!(run.cost_usd, Some(0.4), "{said}");
    }
}

/// A plugin that spawns a background child inheriting stdout/stderr and then
/// exits must not hang `Running::finish` waiting for EOF on those pipes.
#[test]
fn a_plugin_whose_background_child_inherits_pipes_does_not_hang_finish() {
    let tmp = TempDir::new().unwrap();
    let (mut cfg, project) = setup(&tmp);
    let desc = tmp.path().join("desc.json");
    let turn = tmp.path().join("turn.json");
    fs::write(&desc, describe_json("leaker", vec![], vec![])).unwrap();
    fs::write(
        &turn,
        job_json(
            "leaker",
            "sess-leaker",
            Context::Fresh,
            None,
            None,
            "answered despite lingering child",
            Next::Done,
        ),
    )
    .unwrap();
    let plugin = tmp.path().join("leaker-plugin");
    burrow_test::executable(
        &plugin,
        format!(
            "#!/bin/sh\nreq=\"$(cat)\"\nif printf '%s' \"$req\" | grep -q '\"kind\":\"describe\"'; then\n  sleep 60 &\n  cat {}\n  exit 0\nfi\nsleep 60 &\ncat {}\nexit 0\n",
            desc.display(),
            turn.display()
        ),
    );
    cfg.plugins.insert("leaker".into(), plugin);
    let job = make_job(&cfg, &project, "job-leaker");
    let waitdir = job.turn_file("1", TurnFile::Waits);
    fs::create_dir_all(&waitdir).unwrap();
    let prompt = job.prompt("1");
    fs::write(&prompt, "Leak a child\n").unwrap();
    let choice = Choice {
        engine: "leaker".into(),
        model: None,
        effort: None,
    };
    let start = std::time::Instant::now();
    let mut running = spawn(&cfg, &job, "1", &prompt, &waitdir, None, true, "", &choice).unwrap();
    let status = running.child.take().unwrap().wait().unwrap();
    let res = running.finish(status, false, &cfg, &job, "1", 0);
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "finish hung waiting on lingering child: {:?}",
        start.elapsed()
    );
    assert_eq!(res.rc, 0);
    assert_eq!(
        res.last.expect("result recorded")["result"],
        "answered despite lingering child"
    );
}

/// Host `describe` runs with a scrubbed environment (`env_clear` + `PATH`
/// alone, cwd `/`) so offline capability negotiation never exposes host
/// service-user tokens or files.
#[test]
fn host_describe_scrubs_service_user_environment() {
    let tmp = TempDir::new().unwrap();
    let (mut cfg, _) = setup(&tmp);
    let desc = tmp.path().join("scrub-desc.json");
    fs::write(&desc, describe_json("scrub", vec![], vec![])).unwrap();
    let seen = tmp.path().join("scrub-env");
    let plugin = tmp.path().join("scrub-plugin");
    burrow_test::executable(
        &plugin,
        format!(
            "#!/bin/sh\ncat >/dev/null\nprintf 'home=%s\\ncwd=%s\\n' \"${{HOME:-}}\" \"$(pwd)\" > {}\ncat {}\n",
            seen.display(),
            desc.display()
        ),
    );
    cfg.plugins.insert("scrub".into(), plugin.clone());
    describe(&cfg, &plugin, "scrub", None).unwrap();
    let body = fs::read_to_string(&seen).unwrap();
    assert_eq!(body, "home=\ncwd=/\n", "host describe leaked env: {body}");
}

/// A plugin that emits billable `usage` on stderr and then exits 0 with a
/// malformed/mismatched response or empty stdout still records its usage in the
/// turn's stream for accounting without treating the failed run as an answer.
#[test]
fn usage_is_preserved_when_response_is_rejected_or_empty() {
    for (case, stdout_cmd) in [
        ("malformed", "printf '%s\\n' '{not-json'"),
        ("empty", "true"),
    ] {
        let tmp = TempDir::new().unwrap();
        let (mut cfg, project) = setup(&tmp);
        let desc = tmp.path().join("usage-desc.json");
        fs::write(&desc, describe_json("usage-eng", vec![], vec![])).unwrap();
        let plugin = tmp.path().join("usage-plugin");
        burrow_test::executable(
            &plugin,
            format!(
                "#!/bin/sh\nreq=\"$(cat)\"\nif printf '%s' \"$req\" | grep -q '\"kind\":\"describe\"'; then cat {}; exit 0; fi\nprintf '%s\\n' '{{\"event\":\"usage\",\"usage\":{{\"input_tokens\":42,\"output_tokens\":7,\"total_cost_usd\":0.01}}}}' >&2\n{stdout_cmd}\nexit 0\n",
                desc.display()
            ),
        );
        cfg.plugins.insert("usage-eng".into(), plugin);
        let job_id = format!("job-usage-{case}");
        let job = make_job(&cfg, &project, &job_id);
        let waitdir = job.turn_file("1", TurnFile::Waits);
        fs::create_dir_all(&waitdir).unwrap();
        let prompt = job.prompt("1");
        fs::write(&prompt, "Spend then fail\n").unwrap();
        let choice = Choice {
            engine: "usage-eng".into(),
            model: None,
            effort: None,
        };
        let mut running =
            spawn(&cfg, &job, "1", &prompt, &waitdir, None, true, "", &choice).unwrap();
        let status = running.child.take().unwrap().wait().unwrap();
        let res = running.finish(status, false, &cfg, &job, "1", 0);
        assert_eq!(res.rc, 1, "{case}: rejected/empty response must fail");
        assert!(
            res.last.is_none(),
            "{case}: rejected/empty response must not return an answer"
        );
        let stream_text = fs::read_to_string(job.turn_file("1", TurnFile::Jsonl)).unwrap();
        let usage_record: Value = stream_text
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .find(|v| v.get("type").and_then(Value::as_str) == Some("burrow.usage"))
            .unwrap_or_else(|| panic!("{case}: missing burrow.usage record in {stream_text:?}"));
        assert_eq!(usage_record["usage"]["input_tokens"], 42);
        assert_eq!(usage_record["usage"]["output_tokens"], 7);
        assert_eq!(usage_record["total_cost_usd"], 0.01);
    }
}

/// A dual-mode engine (`plugins` for host turns, `vm_plugins` for `--vm`
/// turns) runs its host plugin on host turns and its strict sandbox plugin
/// (with `burrow-grants` enforcement) inside `--vm` turns.
#[test]
fn dual_mode_engine_selects_host_or_vm_plugin_and_enforces_vm_grants() {
    let tmp = TempDir::new().unwrap();
    let (mut cfg, project) = setup(&tmp);
    let (_, calls) = stand_in_sandbox(&tmp, &mut cfg);

    let host_ran = tmp.path().join("dual-host-ran");
    let host_desc = tmp.path().join("dual-host-desc.json");
    let host_turn = tmp.path().join("dual-host-turn.json");
    fs::write(
        &host_desc,
        describe_json_with("dual", vec![], vec![], Sandbox::None),
    )
    .unwrap();
    fs::write(
        &host_turn,
        job_json(
            "dual",
            "host-s1",
            Context::Fresh,
            None,
            None,
            "host ran",
            Next::Done,
        ),
    )
    .unwrap();
    let host_bin = tmp.path().join("dual-host-plugin");
    burrow_test::executable(
        &host_bin,
        format!(
            "#!/bin/sh\ntouch {}\nreq=\"$(cat)\"\nif printf '%s' \"$req\" | grep -q '\"kind\":\"describe\"'; then cat {}; exit 0; fi\ncat {}\n",
            host_ran.display(),
            host_desc.display(),
            host_turn.display()
        ),
    );
    cfg.plugins.insert("dual".into(), host_bin);

    let vm_ran = tmp.path().join("dual-vm-ran");
    let vm_desc = tmp.path().join("dual-vm-desc.json");
    fs::write(
        &vm_desc,
        describe_json_with("dual", vec![], vec![], Sandbox::Needs),
    )
    .unwrap();
    let vm_bin = tmp.path().join("dual-vm-plugin");
    burrow_test::executable(
        &vm_bin,
        format!(
            "#!/bin/sh\ntouch {}\ncat {}\n",
            vm_ran.display(),
            vm_desc.display()
        ),
    );
    cfg.vm_plugins.insert(
        "dual".into(),
        crate::config::VmPlugin {
            plugin: vm_bin,
            capabilities: vec!["workspace".into()],
        },
    );

    let job = make_job(&cfg, &project, "job-dual");
    let waitdir = job.turn_file("1", TurnFile::Waits);
    fs::create_dir_all(&waitdir).unwrap();
    let prompt = job.prompt("1");
    fs::write(&prompt, "Dual mode\n").unwrap();
    let choice = Choice {
        engine: "dual".into(),
        model: None,
        effort: None,
    };

    // Host turn runs the host plugin and never touches the VM plugin.
    let mut running = spawn(&cfg, &job, "1", &prompt, &waitdir, None, true, "", &choice).unwrap();
    let status = running.child.take().unwrap().wait().unwrap();
    let res = running.finish(status, false, &cfg, &job, "1", 0);
    assert_eq!(res.rc, 0);
    assert!(host_ran.exists());
    assert!(!vm_ran.exists());

    // VM turn uses vm_plugins and enforces its owner-approved capability list
    // before launching (here missing required VM capabilities).
    crate::job::Request {
        project: project.to_string_lossy().into_owned(),
        agent: Some("dual-1".into()),
        agent_engine: Some("dual".into()),
        ..Default::default()
    }
    .save(&job)
    .unwrap();
    let refused = match spawn(
        &cfg,
        &job,
        "2",
        &prompt,
        &waitdir,
        Some("dual-1"),
        true,
        "",
        &choice,
    ) {
        Err(SpawnError::Refused(r)) => r,
        Err(SpawnError::Failed(e)) => panic!("expected VM capability refusal, got Failed({e})"),
        Ok(_) => panic!("expected VM capability refusal, got Ok"),
    };
    assert_eq!(refused.kind, RefusalKind::Misplaced);
    assert!(!vm_ran.exists());
    assert!(!calls.exists());
}
