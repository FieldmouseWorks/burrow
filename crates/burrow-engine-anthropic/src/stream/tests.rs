use std::fs::{self, File};
use std::io::Write;

use super::*;
use crate::claude;

#[test]
fn partial_first_id_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let stream = dir.path().join("1.jsonl");
    fs::write(
        &stream,
        r#"{"type":"system","subtype":"init","session_id":"first"}"#,
    )
    .unwrap();
    let mut watcher = Watcher::new(&stream, 0);
    watcher.look(false);
    assert_eq!(watcher.session, None);
    watcher.look(true);
    assert_eq!(watcher.session.as_deref(), Some("first"));
    assert!(!watcher.answered);
    assert_eq!(stream_session(&stream).as_deref(), Some("first"));
    assert!(stream_names_session(&stream, 0));
    assert!(!stream_names_session(
        &stream,
        fs::metadata(&stream).unwrap().len()
    ));
}

#[test]
fn results_and_empty_resume_records() {
    let dir = tempfile::tempdir().unwrap();
    let s = dir.path().join("1.jsonl");
    fs::write(
        &s,
        concat!(
            "{\"type\":\"system\",\"session_id\":\"s\"}\n",
            "{\"type\":\"result\",\"session_id\":\"s\",\"result\":\"waiting on CI\",\"is_error\":false}\n",
            "{\"type\":\"result\",\"session_id\":\"s\",\"is_error\":true,\"errors\":[\"boom\"]}\n",
            "{\"type\":\"result\",\"session_id\":\"t\",\"result\":\"the build passed\",\"is_error\":false}\n",
        ),
    )
    .unwrap();
    assert_eq!(answer(&s), "waiting on CI\n\nthe build passed");
    assert_eq!(last_result(&s, 0).unwrap()["session_id"], "t");
    let len = fs::metadata(&s).unwrap().len();
    assert_eq!(last_result(&s, len), None);
    fs::write(&s, "{\"type\":\"result\", nope\n").unwrap();
    assert_eq!(answer(&s), "");
    assert_eq!(last_result(&s, 0), None);
    assert_eq!(answer(&dir.path().join("none")), "");
    let empty = "{\"type\":\"result\",\"num_turns\":0,\"result\":\"\",\"is_error\":false}";
    assert!(result_of(empty).is_none());
    assert!(result_of("{\"type\":\"result\",\"num_turns\":0,\"is_error\":false}").is_none());
    assert!(
        result_of("{\"type\":\"result\",\"num_turns\":3,\"result\":\"\",\"is_error\":false}")
            .is_some()
    );
    assert!(result_of("{\"type\":\"result\",\"result\":\"\"}").is_some());
    assert!(
        result_of("{\"type\":\"result\",\"num_turns\":0,\"result\":\"\",\"is_error\":true}")
            .is_some()
    );
    assert!(result_of("{\"type\":\"result\",\"num_turns\":0,\"result\":\"hi\"}").is_some());
    fs::write(&s, format!("{empty}\n")).unwrap();
    assert_eq!(last_result(&s, 0), None);
}

#[test]
fn large_transcripts_keep_results_sessions_offsets_and_refusals() {
    let dir = tempfile::tempdir().unwrap();
    let stream = dir.path().join("1.jsonl");
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
        let mut watcher = Watcher::new(&stream, from);
        watcher.look(true);
        assert_eq!(watcher.session.as_deref(), Some("large-session"));
        assert!(watcher.answered);
    }
    assert_eq!(answer(&stream), "Which release?");
    file.write_all(b"{\"type\":\"system\",\"rate_limit_info\":{\"status\":\"rejected\",\"rateLimitType\":\"seven_day\",\"resetsAt\":1234567890}}\n{\"type\":\"result\",\"is_error\":true,\"api_error\":\"usage_limit_reached\",\"result\":\"plan is spent\"}\n").unwrap();
    file.flush().unwrap();
    let last = last_result(&stream, offset).unwrap();
    assert_eq!(
        claude::refused_file(&last, &stream, offset, 0).unwrap().0,
        Some(1234567890)
    );
}
