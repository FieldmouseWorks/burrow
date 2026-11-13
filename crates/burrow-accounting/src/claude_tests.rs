use super::*;
use serde_json::json;
fn result(input: u64, cost: f64) -> Value {
    json!({"type":"result","session_id":"s","usage":{"input_tokens":input,"output_tokens":3,"cache_read_input_tokens":2,"cache_creation_input_tokens":1},"total_cost_usd":cost})
}
fn message(input: u64) -> Value {
    json!({"type":"assistant","session_id":"s","message":{"id":"m","usage":{"input_tokens":input,"output_tokens":99,"cache_read_input_tokens":4,"cache_creation_input_tokens":5}}})
}
fn crash() -> Value {
    json!({"type":"result","subtype":"error_during_execution","session_id":"s","is_error":true,"usage":{"input_tokens":0,"output_tokens":0},"total_cost_usd":0})
}
#[test]
fn tokens_are_per_invocation_and_only_cost_uses_the_session_baseline() {
    let mut sessions = BTreeMap::new();
    let a = decode(
        vec![
            json!({"type":"burrow.run"}),
            json!({"type":"system","subtype":"init","session_id":"s"}),
            result(100, 1.5),
        ],
        &mut sessions,
        "1",
    );
    let b = decode(vec![result(80, 2.0), result(80, 2.0)], &mut sessions, "2");
    assert_eq!(a.tokens, [100, 3, 2, 1]);
    assert!(!a.tokens_partial && !a.cost_partial);
    assert_eq!(b.tokens, [160, 6, 4, 2]);
    assert_eq!(b.cost, Some(0.5));
    assert!(!b.tokens_partial && !b.cost_partial);
}
#[test]
fn zeroed_crash_is_partial_and_does_not_erase_an_earlier_turns_baseline() {
    let mut sessions = BTreeMap::new();
    decode(vec![result(100, 1.5)], &mut sessions, "1");
    let bad = decode(vec![message(7), message(7), crash()], &mut sessions, "2");
    let next = decode(vec![result(80, 2.0)], &mut sessions, "3");
    assert_eq!(next.tokens, [80, 3, 2, 1]);
    assert_eq!(next.cost, Some(0.5));
    assert!(next.prior_gap);
    assert_eq!(bad.tokens, [7, 0, 4, 5]);
    assert!(bad.tokens_partial && bad.cost_partial);
    let mut error = result(6, 2.25);
    error["subtype"] = json!("error_max_turns");
    error["is_error"] = json!(true);
    let valid_error = decode(vec![error], &mut sessions, "4");
    assert_eq!(valid_error.tokens, [6, 3, 2, 1]);
    assert_eq!(valid_error.cost, Some(0.25));
    assert!(!valid_error.tokens_partial && !valid_error.cost_partial);
}
#[test]
fn earlier_result_does_not_hide_a_later_unfinished_run() {
    for init in [
        json!({"type":"burrow.run"}),
        json!({"type":"system","subtype":"init","session_id":"s"}),
    ] {
        let mut sessions = BTreeMap::new();
        let report = decode(
            vec![
                result(100, 1.5),
                init,
                message(7),
                message(7),
                json!({"type":"assistant","session_id":"s","message":{"id":"m","usage":{"output_tokens":99}}}),
            ],
            &mut sessions,
            "1",
        );
        assert_eq!(report.tokens, [107, 3, 6, 6]);
        assert_eq!(report.cost, Some(1.5));
        assert!(report.tokens_partial && report.cost_partial);
        assert!(report.show(None).contains("additional cost unavailable"));
    }
    let report = decode(
        vec![result(100, 1.5), message(7)],
        &mut BTreeMap::new(),
        "old-stream",
    );
    assert_eq!(report.tokens[0], 107);
    assert!(report.tokens_partial && report.cost_partial);
    let before_init = decode(
        vec![result(100, 1.5), json!({"type":"burrow.run"})],
        &mut BTreeMap::new(),
        "1",
    );
    assert_eq!(before_init.tokens, [100, 3, 2, 1]);
    assert!(before_init.tokens_partial && before_init.cost_partial);
}
#[test]
fn multiple_appended_runs_recover_after_a_crash_without_counting_old_cost_twice() {
    let report = decode(
        vec![
            result(100, 1.5),
            json!({"type":"burrow.run"}),
            message(7),
            crash(),
            json!({"type":"burrow.run"}),
            result(80, 2.0),
        ],
        &mut BTreeMap::new(),
        "1",
    );
    assert_eq!(report.tokens, [187, 6, 8, 7]);
    assert_eq!(report.cost, Some(2.0));
    assert!(report.tokens_partial && report.cost_partial && report.prior_gap);
}

#[test]
fn crash_fields_are_recovered_independently_without_discarding_valid_fields() {
    let mut error = result(8, 0.0);
    error["subtype"] = json!("error_during_execution");
    let tokens = decode(vec![error], &mut BTreeMap::new(), "1");
    assert_eq!(tokens.tokens, [8, 3, 2, 1]);
    assert!(!tokens.tokens_partial && tokens.cost_partial);
    assert!(tokens.show(None).contains("known-cost=$0.000000"));
    let mut error = crash();
    error["total_cost_usd"] = json!(1.5);
    let cost = decode(vec![message(7), error], &mut BTreeMap::new(), "2");
    assert_eq!(cost.tokens, [7, 0, 4, 5]);
    assert_eq!(cost.cost, Some(1.5));
    assert!(cost.tokens_partial && !cost.cost_partial);
    let mut sessions = BTreeMap::new();
    decode(vec![result(100, 1.5)], &mut sessions, "1");
    let regressed = decode(vec![result(80, 1.0)], &mut sessions, "2");
    assert_eq!(regressed.tokens, [80, 3, 2, 1]);
    assert!(regressed.cost_partial);
    let next = decode(vec![result(80, 2.0)], &mut sessions, "3");
    assert_eq!(next.cost, Some(0.5));
}

#[test]
fn recovery_tracks_every_gap_including_appended_runs_and_separate_jobs() {
    let mut sessions = BTreeMap::new();
    decode(vec![result(100, 1.5)], &mut sessions, "job-a/1");
    let first = decode(vec![crash()], &mut sessions, "job-a/2");
    let second = decode(vec![crash(), result(80, 2.0)], &mut sessions, "job-a/3");
    assert_eq!(second.recovered.len(), 2);
    assert!(first.missing.is_subset(&second.recovered));
    assert!(second.missing.is_subset(&second.recovered));
    let other = decode(vec![crash()], &mut BTreeMap::new(), "job-b/2");
    assert!(other.missing.is_disjoint(&second.recovered));
}
