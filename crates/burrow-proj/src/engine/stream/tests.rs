use super::*;
use crate::config::{Config, tests::example};

#[test]
fn partial_first_id_is_kept_and_resumed() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg: Config = serde_json::from_value(example()).unwrap();
    cfg.jobs = dir.path().into();
    let job = Job::new(&cfg, "job");
    fs::create_dir(&job.dir).unwrap();
    let stream = job.turn("1", "jsonl");
    fs::write(
        &stream,
        r#"{"type":"system","subtype":"init","session_id":"first"}"#,
    )
    .unwrap();
    let mut said = Said::new(&job, "1", 0);
    said.look(false);
    assert!(!said.named);
    said.look(true);
    assert!(said.named && !said.answered);
    assert_eq!(resume(&job, Engine::Claude).as_deref(), Some("first"));
    assert_eq!(read(&job.path("session")).as_deref(), Some("first"));
    assert!(stream_names_session(&stream, 0));
    assert!(!stream_names_session(
        &stream,
        fs::metadata(&stream).unwrap().len()
    ));
}

#[test]
fn large_transcripts_keep_results_sessions_offsets_questions_and_handoffs() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let mut cfg: Config = serde_json::from_value(example()).unwrap();
    cfg.jobs = dir.path().join("jobs");
    let job = Job::new(&cfg, "large");
    fs::create_dir_all(&job.dir).unwrap();
    job.set_turns(1).unwrap();
    let stream = job.turn("1", "jsonl");
    let mut file = std::io::BufWriter::new(File::create(&stream).unwrap());
    let record = format!(
        "{{\"type\":\"progress\",\"text\":\"{}\"}}\n",
        "x".repeat(4096)
    );
    for _ in 0..17000 {
        file.write_all(record.as_bytes()).unwrap();
    }
    file.flush().unwrap();
    let offset = fs::metadata(&stream).unwrap().len();
    file.write_all(b"{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"large-session\"}\n{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"checking\"}]}}\n{\"type\":\"result\",\"is_error\":false,\"result\":\"Which release?\"}\n").unwrap();
    file.flush().unwrap();
    assert!(offset > 64 * 1024 * 1024);
    for from in [0, offset] {
        assert!(stream_names_session(&stream, from));
        assert_eq!(
            last_result(&stream, from).unwrap()["result"],
            "Which release?"
        );
        let mut said = Said::new(&job, "1", from);
        said.look(true);
        assert!(said.named && said.answered);
    }
    assert_eq!(answer(&stream), "Which release?");
    fs::write(job.path("project"), dir.path().to_str().unwrap()).unwrap();
    fs::write(job.turn("1", "engine"), "claude").unwrap();
    fs::write(job.turn("1", "end"), "ask").unwrap();
    fs::write(job.turn("1", "result"), answer(&stream)).unwrap();
    crate::ask::record(
        &job,
        &crate::ask::Recommendation {
            default: "keep it".into(),
            one_way: false,
            about: None,
        },
    )
    .unwrap();
    crate::ask::finish(&cfg, &job, "ask").unwrap();
    assert!(crate::ask::load(&job).unwrap().is_some());
    assert!(
        crate::cmd::told(&cfg, job.id, "1")
            .unwrap()
            .contains("Which release?")
    );
    fs::write(job.path("prompt.2"), "continue the conversation").unwrap();
    // The conversation is lost: the next turn starts a new one, handed
    // what it missed.
    for lost in [
        job.path("session"),
        job.file(crate::record::JobFile::SessionOf(Engine::Claude)),
    ] {
        let _ = fs::remove_file(lost);
    }
    assert_eq!(resume(&job, Engine::Claude), None);
    let prompt = crate::engine::first_prompt(&job, "2", Engine::Claude).unwrap();
    // Handoffs retain the terminal answer rather than the bulk progress.
    assert!(
        fs::read_to_string(prompt)
            .unwrap()
            .contains("Which release?")
    );
    file.write_all(b"{\"type\":\"system\",\"rate_limit_info\":{\"status\":\"rejected\",\"rateLimitType\":\"seven_day\",\"resetsAt\":1234567890}}\n{\"type\":\"result\",\"is_error\":true,\"api_error\":\"usage_limit_reached\",\"result\":\"plan is spent\"}\n").unwrap();
    file.flush().unwrap();
    assert_eq!(
        crate::state_reads::copy(&stream, &mut std::io::sink()).unwrap(),
        fs::metadata(&stream).unwrap().len()
    );
    let last = last_result(&stream, offset).unwrap();
    assert_eq!(
        crate::engine::refused_file(Engine::Claude, &last, &stream, offset, 0)
            .unwrap()
            .0,
        Some(1234567890)
    );
}
