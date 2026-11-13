use super::*;
use serde_json::json;
fn completed(input: u64, cached: u64, output: u64) -> Value {
    json!({"type":"turn.completed","usage":{"input_tokens":input,"cached_input_tokens":cached,"cache_write_input_tokens":0,"output_tokens":output,"reasoning_output_tokens":1}})
}
fn run(thread: &str, total: Value) -> Vec<Value> {
    vec![
        json!({"type":"burrow.run","at":1}),
        json!({"type":"thread.started","thread_id":thread}),
        json!({"type":"turn.started"}),
        total,
    ]
}
#[test]
fn a_resumed_thread_counts_each_turn_once() {
    let mut threads = BTreeMap::new();
    // Turn 1 runs twice (a retry): its last total covers both runs.
    let mut first = run("t", completed(100, 60, 10));
    first.extend(run("t", completed(150, 90, 12)));
    let a = decode(first, &mut threads, "turn-1");
    // Turn 2 resumes the thread: its total includes turn 1's.
    let b = decode(run("t", completed(400, 300, 30)), &mut threads, "turn-2");
    // Turn 3 starts a thread of its own.
    let c = decode(run("u", completed(50, 0, 5)), &mut threads, "turn-3");
    assert_eq!(a.tokens, [60, 12, 90, 0]);
    assert_eq!(b.tokens, [40, 18, 210, 0]);
    assert_eq!(c.tokens, [50, 5, 0, 0]);
    // The three turns sum to the two threads' last totals, nothing twice.
    let sum: Vec<u64> = (0..4)
        .map(|i| a.tokens[i] + b.tokens[i] + c.tokens[i])
        .collect();
    assert_eq!(sum, [100 + 50, 30 + 5, 300, 0]);
    for r in [&a, &b, &c] {
        assert!(r.cost.is_none() && r.context.is_none() && !r.tokens_partial && !r.prior_gap);
    }
    assert_eq!(
        b.show(None),
        "input=40 output=18 cache-read=210 cache-write=0 cost unpriced context unavailable"
    );
}
#[test]
fn cache_writes_are_part_of_input() {
    let mut threads = BTreeMap::new();
    let r = decode(
        run(
            "t",
            json!({"type":"turn.completed","usage":{"input_tokens":100,"cached_input_tokens":30,"cache_write_input_tokens":20,"output_tokens":5}}),
        ),
        &mut threads,
        "turn-1",
    );
    assert_eq!(r.tokens, [50, 5, 30, 20]);
}
#[test]
fn an_unreported_run_marks_the_next_total() {
    let mut threads = BTreeMap::new();
    decode(run("t", completed(100, 0, 10)), &mut threads, "turn-1");
    // Killed mid-turn: no total.
    let killed = decode(
        vec![
            json!({"type":"burrow.run","at":1}),
            json!({"type":"thread.started","thread_id":"t"}),
            json!({"type":"turn.started"}),
        ],
        &mut threads,
        "turn-2",
    );
    assert_eq!(killed.tokens, [0; 4]);
    assert_eq!(
        killed.show(None),
        "input=0 output=0 cache-read=0 cache-write=0 cost unpriced context unavailable; partial tokens (a run reported no total)"
    );
    let next = decode(run("t", completed(300, 0, 20)), &mut threads, "turn-3");
    assert_eq!(next.tokens, [200, 10, 0, 0]);
    assert!(next.prior_gap && !next.tokens_partial);
    // The killed run's gap is matched to the total that recovers it.
    assert_eq!(killed.missing, BTreeSet::from(["turn-2:0".to_string()]));
    assert_eq!(next.recovered, killed.missing);
    assert!(next.missing.is_empty());
    assert!(
        next.show(None)
            .ends_with("; tokens may include unreported earlier runs")
    );
    // A run cut off before its total, then retried within the same turn.
    let retried = {
        let mut v = vec![
            json!({"type":"burrow.run","at":1}),
            json!({"type":"thread.started","thread_id":"t"}),
            json!({"type":"turn.started"}),
        ];
        v.extend(run("t", completed(350, 0, 25)));
        decode(v, &mut threads, "turn-4")
    };
    assert_eq!(retried.tokens, [50, 5, 0, 0]);
    assert!(retried.tokens_partial);
}
#[test]
fn a_regressed_or_missing_total_keeps_the_last_one() {
    let mut threads = BTreeMap::new();
    decode(run("t", completed(100, 0, 10)), &mut threads, "turn-1");
    let regressed = decode(run("t", completed(40, 0, 2)), &mut threads, "turn-2");
    assert_eq!(regressed.tokens, [0; 4]);
    assert!(regressed.tokens_partial);
    let missing = decode(
        run("t", json!({"type":"turn.completed"})),
        &mut threads,
        "turn-3",
    );
    assert_eq!(missing.tokens, [0; 4]);
    assert!(missing.tokens_partial);
    let next = decode(run("t", completed(130, 0, 13)), &mut threads, "turn-4");
    assert_eq!(next.tokens, [30, 3, 0, 0]);
    assert!(next.prior_gap);
}
#[test]
fn a_stream_with_no_total_is_partial() {
    let mut threads = BTreeMap::new();
    let r = decode(vec![json!({})], &mut threads, "turn-1");
    assert!(r.tokens_partial && r.cost.is_none());
    let failed = decode(
        vec![
            json!({"type":"thread.started","thread_id":"t"}),
            json!({"type":"turn.started"}),
            json!({"type":"turn.failed","error":{"message":"x"}}),
        ],
        &mut threads,
        "turn-2",
    );
    assert!(failed.tokens_partial);
}
