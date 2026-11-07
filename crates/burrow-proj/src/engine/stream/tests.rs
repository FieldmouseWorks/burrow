use super::*;
use crate::config::{Config, tests::example};

#[test]
fn only_terminal_events_answer_and_errors_stay_errors() {
    let mut decoder = Decoder::default();
    assert!(decoder.line(r#"{"type":"turn.started"}"#).is_none());
    assert!(
        decoder
            .line(r#"{"type":"item.completed","item":{"type":"agent_message","text":"checking"}}"#)
            .is_none()
    );
    assert!(
        decoder
            .line(
                r#"{"type":"item.completed","item":{"type":"agent_message","text":"the answer"}}"#
            )
            .is_none()
    );
    assert_eq!(
        decoder.line(r#"{"type":"turn.completed"}"#).unwrap()["result"],
        "the answer"
    );
    assert!(is_error(
        &decoder
            .line(r#"{"type":"error","message":"usage limit; try again in 20 minutes"}"#)
            .unwrap()
    ));
    assert!(is_error(
        &decoder.line(r#"{"type":"turn.completed"}"#).unwrap()
    ));
    assert!(is_error(
        &decoder
            .line(r#"{"type":"turn.failed","error":{"message":"not logged in"}}"#)
            .unwrap()
    ));
}

#[test]
fn partial_first_id_is_kept_and_resumed_only_by_its_engine() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg: Config = serde_json::from_value(example()).unwrap();
    cfg.jobs = dir.path().into();
    let job = Job::new(&cfg, "job");
    fs::create_dir(&job.dir).unwrap();
    let stream = job.turn("1", "jsonl");
    fs::write(&stream, r#"{"type":"thread.started","thread_id":"first"}"#).unwrap();
    let mut said = Said::new(&job, "1", 0);
    said.look(false);
    assert!(!said.named);
    said.look(true);
    assert!(said.named && !said.answered);
    assert_eq!(resume(&job, Engine::Codex).as_deref(), Some("first"));
    assert_eq!(resume(&job, Engine::Claude), None);
    assert_eq!(read(&job.path("session")).as_deref(), Some("first"));
    assert!(stream_names_session(&stream, 0));
    assert!(!stream_names_session(
        &stream,
        fs::metadata(&stream).unwrap().len()
    ));
}

#[test]
fn final_answer_is_last_agent_message_and_limit_uses_shared_parser() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stream");
    let text = concat!(
        "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"checking\"}}\n",
        "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"done\"}}\n",
        "{\"type\":\"turn.completed\"}\n"
    );
    fs::write(&path, text).unwrap();
    assert_eq!(answer(&path), "done");
    assert!(!is_error(&last_result(&path, 0).unwrap()));
    assert_eq!(last_result(&path, text.len() as u64), None);
    let error = json!({"is_error": true, "result": "usage_limit_reached; try again in 20 minutes"});
    assert_eq!(
        super::super::refused(Engine::Codex, &error, "", 100)
            .unwrap()
            .0,
        Some(1300)
    );
}

#[test]
fn switching_to_codex_keeps_a_legacy_claude_session() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg: Config = serde_json::from_value(example()).unwrap();
    cfg.jobs = dir.path().into();
    let job = Job::new(&cfg, "job");
    fs::create_dir(&job.dir).unwrap();
    fs::write(job.path("session"), "legacy\n").unwrap();
    fs::write(job.turn("1", "jsonl"), "{\"thread_id\":\"new\"}\n").unwrap();
    Said::new(&job, "1", 0).look(true);
    assert_eq!(resume(&job, Engine::Claude).as_deref(), Some("legacy"));
    assert_eq!(resume(&job, Engine::Codex).as_deref(), Some("new"));
}

#[test]
fn startup_errors_and_nonzero_exits_cannot_pass_as_success() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stream");
    fs::write(&path, "").unwrap();
    let failure = super::super::ended(Engine::Codex, &path, 0, Some(1), "Not logged in").unwrap();
    assert!(is_error(&failure));
    assert!(burrow_codex::logged_out(
        failure["result"].as_str().unwrap()
    ));
    fs::write(&path, "{\"type\":\"turn.completed\"}\n").unwrap();
    assert!(is_error(
        &super::super::ended(Engine::Codex, &path, 0, Some(1), "").unwrap()
    ));
}

#[test]
fn a_quiet_or_timed_out_process_is_left_to_the_turns_outcome_logic() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stream");
    fs::write(
        &path,
        "{\"type\":\"thread.started\",\"thread_id\":\"first\"}\n",
    )
    .unwrap();
    assert!(super::super::ended(Engine::Codex, &path, 0, None, "").is_none());
    fs::write(&path, "{\"type\":\"turn.completed\"}\n").unwrap();
    // A final answer that cut off background work can be corrected once,
    // through the same lifecycle as a Claude answer that lingered.
    assert!(!is_error(
        &super::super::ended(Engine::Codex, &path, 0, None, "").unwrap()
    ));
}
