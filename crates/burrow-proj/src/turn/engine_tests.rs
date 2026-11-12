use super::*;
use crate::config::tests::example;

/// A run killed for memory before it answered ends with nothing said,
/// for either engine, so the turn can carry it on in its session; a
/// failed exit of Codex's own is still its error, and an answer is
/// still the answer.
#[test]
fn a_run_killed_for_memory_says_nothing_for_either_engine() {
    let dir = tempfile::tempdir().unwrap();
    let stream = dir.path().join("1.jsonl");
    let ended = |engine, text: &str, exit| {
        fs::write(&stream, text).unwrap();
        engine::ended(engine, &stream, 0, exit, "out of memory")
    };
    let codex = "{\"type\":\"thread.started\",\"thread_id\":\"t\"}\n{\"type\":\"turn.started\"}\n";
    let claude = "{\"type\":\"system\",\"session_id\":\"s\"}\n";
    for (engine, text) in [(Engine::Codex, codex), (Engine::Claude, claude)] {
        fs::write(&stream, text).unwrap();
        assert!(engine::stream_names_session(&stream, 0), "{text}");
        assert_eq!(
            ended(engine, text, exit(137, false, false, false, true)),
            None
        );
        // Cut, quiet or late, likewise.
        for (cut, quiet, late) in [
            (true, false, false),
            (false, true, false),
            (false, false, true),
        ] {
            assert_eq!(
                ended(engine, text, exit(137, cut, quiet, late, false)),
                None
            );
        }
    }
    // Codex's own failed exit, not killed for memory: its error.
    let error = ended(Engine::Codex, codex, exit(1, false, false, false, false)).unwrap();
    assert!(is_error(&error));
    assert_eq!(error["result"], "out of memory");
    // An answer it gave before the kill is still its answer.
    let answered = format!(
        "{codex}{{\"type\":\"item.completed\",\"item\":{{\"type\":\"agent_message\",\"text\":\"done\"}}}}\n{{\"type\":\"turn.completed\"}}\n"
    );
    let last = ended(
        Engine::Codex,
        &answered,
        exit(137, false, false, false, true),
    )
    .unwrap();
    assert_eq!(last["result"], "done");
}

#[test]
fn invalid_roles_and_codex_vms_fail_with_an_outcome_before_taking_a_slot() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg: Config = serde_json::from_value(example()).unwrap();
    cfg.jobs = dir.path().join("jobs");
    cfg.scratch = dir.path().join("scratch");
    cfg.use_overlay(dir.path());
    let job = Job::new(&cfg, "job");
    fs::create_dir_all(&job.dir).unwrap();
    job.set_turns(1).unwrap();
    fs::write(job.path("role"), "work").unwrap();
    fs::write(job.path("agent"), "sandbox").unwrap();
    for (line, expected) in [
        ("work unknown", "line 1: work unknown"),
        ("work codex", "codex cannot run a --vm job"),
    ] {
        let (role, bad) = line.split_once(' ').unwrap();
        // The door refuses a bad engine, so a hand-edited overlay carries it.
        fs::write(
            dir.path().join("overlay.json"),
            serde_json::json!({"engines": {"roles": {role: bad}}}).to_string(),
        )
        .unwrap();
        assert!(exec(&cfg, "job", "normal").is_err());
        assert!(read(&job.turn("1", "outcome")).unwrap().contains(expected));
        assert!(!job.turn("1", "slot").exists());
    }
}
