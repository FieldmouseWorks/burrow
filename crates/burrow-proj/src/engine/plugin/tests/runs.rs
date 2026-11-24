//! Where a turn's plugin may run, and what a run must answer.

use std::fs;

use conary_core::plugin::Sandbox;
use tempfile::TempDir;

use super::super::launch::placed;
use super::*;

/// A plugin that records whether its job ran, after answering `describe`.
fn recording_plugin(tmp: &TempDir, cfg: &mut Config, engine: &str, sandbox: Sandbox) -> PathBuf {
    let desc = tmp.path().join(format!("{engine}-desc.json"));
    fs::write(&desc, describe_json_with(engine, vec![], vec![], sandbox)).unwrap();
    let ran = tmp.path().join(format!("{engine}-ran"));
    let plugin = tmp.path().join(format!("{engine}-plugin"));
    burrow_test::executable(
        &plugin,
        format!(
            "#!/usr/bin/env bash\nreq=\"$(cat)\"\nif printf '%s' \"$req\" | grep -q '\"kind\":\"describe\"'; then cat {}; exit 0; fi\ntouch {}\nexit 1\n",
            desc.display(),
            ran.display()
        ),
    );
    cfg.plugins.insert(engine.into(), plugin);
    ran
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
        kind(placed(None, None, "e", false, &needs)),
        Some(RefusalKind::Misplaced)
    );
    assert_eq!(kind(placed(None, None, "e", false, &plain)), None);
    assert_eq!(kind(placed(None, None, "e", true, &plain)), None);
    // In the sandbox made for it, or one from before that was kept.
    assert_eq!(
        kind(placed(Some("vm"), Some("e"), "e", false, &needs)),
        None
    );
    assert_eq!(kind(placed(Some("vm"), None, "e", false, &needs)), None);
    // In a sandbox made for another engine, and on the credit in any.
    assert_eq!(
        kind(placed(Some("vm"), Some("other"), "e", false, &needs)),
        Some(RefusalKind::Misplaced)
    );
    assert_eq!(
        kind(placed(Some("vm"), Some("e"), "e", true, &plain)),
        Some(RefusalKind::CreditExhausted)
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
                "#!/usr/bin/env bash\nreq=\"$(cat)\"\nif printf '%s' \"$req\" | grep -q '\"kind\":\"describe\"'; then cat {}; exit 0; fi\nprintf '%s' \"${{PROJ_CREDIT_LEDGER_FILE:-none}}\" > {}\nexit 1\n",
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
                "#!/usr/bin/env bash\nreq=\"$(cat)\"\nif printf '%s' \"$req\" | grep -q '\"kind\":\"describe\"'; then cat {}; exit 0; fi\ncat {} >&2\nexit 0\n",
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
                "#!/usr/bin/env bash\nreq=\"$(cat)\"\nif printf '%s' \"$req\" | grep -q '\"kind\":\"describe\"'; then cat {}; exit 0; fi\ncat {} >&2\nexit 3\n",
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
                "#!/usr/bin/env bash\nreq=\"$(cat)\"\nif printf '%s' \"$req\" | grep -q '\"kind\":\"describe\"'; then cat {}; exit 0; fi\ncat {} >&2\nexit 3\n",
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
