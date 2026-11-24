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
    let described = |sandbox, meters: Vec<String>| {
        parse_describe(
            "e",
            describe_json_with("e", vec![], meters, sandbox).as_bytes(),
        )
        .unwrap()
    };
    let needs = described(Sandbox::Needs, vec![]);
    let plain = described(Sandbox::None, vec![]);
    let credit = described(Sandbox::None, vec!["credit".into()]);
    let kind = |r: Result<(), PluginRefusal>| r.err().map(|r| r.kind);
    // On the host: only an engine that needs a sandbox is refused.
    assert_eq!(
        kind(placed(None, None, "e", &needs)),
        Some(RefusalKind::Misplaced)
    );
    assert_eq!(kind(placed(None, None, "e", &plain)), None);
    // In the sandbox made for it, or one from before that was kept.
    assert_eq!(kind(placed(Some("vm"), Some("e"), "e", &needs)), None);
    assert_eq!(kind(placed(Some("vm"), None, "e", &needs)), None);
    // In a sandbox made for another engine, and on a credit in any.
    assert_eq!(
        kind(placed(Some("vm"), Some("other"), "e", &needs)),
        Some(RefusalKind::Misplaced)
    );
    assert_eq!(
        kind(placed(Some("vm"), Some("e"), "e", &credit)),
        Some(RefusalKind::CreditExhausted)
    );
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
