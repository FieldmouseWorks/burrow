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
    let a = decode(first.into_iter(), &mut threads, "turn-1");
    // Turn 2 resumes the thread: its total includes turn 1's.
    let b = decode(
        run("t", completed(400, 300, 30)).into_iter(),
        &mut threads,
        "turn-2",
    );
    // Turn 3 starts a thread of its own.
    let c = decode(
        run("u", completed(50, 0, 5)).into_iter(),
        &mut threads,
        "turn-3",
    );
    assert_eq!(a.tokens, [60, 12, 90, 0]);
    assert_eq!(b.tokens, [40, 18, 210, 0]);
    assert_eq!(c.tokens, [50, 5, 0, 0]);
    // The three turns sum to the two threads' last totals, nothing twice.
    let sum: Vec<u64> = (0..4)
        .map(|i| a.tokens[i] + b.tokens[i] + c.tokens[i])
        .collect();
    assert_eq!(sum, [100 + 50, 30 + 5, 300, 0]);
    for r in [&a, &b, &c] {
        assert!(r.unpriced && !r.tokens_partial && !r.prior_gap);
        assert_eq!(r.cost, 0.0);
    }
    assert_eq!(
        b.show(),
        "input=40 output=18 cache-read=210 cache-write=0 cost unpriced"
    );
}
#[test]
fn cache_writes_are_part_of_input() {
    let mut threads = BTreeMap::new();
    let r = decode(
        run(
            "t",
            json!({"type":"turn.completed","usage":{"input_tokens":100,"cached_input_tokens":30,"cache_write_input_tokens":20,"output_tokens":5}}),
        )
        .into_iter(),
        &mut threads,
        "turn-1",
    );
    assert_eq!(r.tokens, [50, 5, 30, 20]);
}
#[test]
fn an_unreported_run_marks_the_next_total() {
    let mut threads = BTreeMap::new();
    decode(
        run("t", completed(100, 0, 10)).into_iter(),
        &mut threads,
        "turn-1",
    );
    // Killed mid-turn: no total.
    let killed = decode(
        vec![
            json!({"type":"burrow.run","at":1}),
            json!({"type":"thread.started","thread_id":"t"}),
            json!({"type":"turn.started"}),
        ]
        .into_iter(),
        &mut threads,
        "turn-2",
    );
    assert_eq!(killed.tokens, [0; 4]);
    assert_eq!(
        killed.show(),
        "input=0 output=0 cache-read=0 cache-write=0 cost unpriced; partial tokens (a run reported no total)"
    );
    let next = decode(
        run("t", completed(300, 0, 20)).into_iter(),
        &mut threads,
        "turn-3",
    );
    assert_eq!(next.tokens, [200, 10, 0, 0]);
    assert!(next.prior_gap && !next.tokens_partial);
    // The killed run's gap is matched to the total that recovers it.
    assert_eq!(killed.missing, BTreeSet::from(["turn-2:0".to_string()]));
    assert_eq!(next.recovered, killed.missing);
    assert!(next.missing.is_empty());
    assert!(
        next.show()
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
        decode(v.into_iter(), &mut threads, "turn-4")
    };
    assert_eq!(retried.tokens, [50, 5, 0, 0]);
    assert!(retried.tokens_partial);
}
#[test]
fn a_regressed_or_missing_total_keeps_the_last_one() {
    let mut threads = BTreeMap::new();
    decode(
        run("t", completed(100, 0, 10)).into_iter(),
        &mut threads,
        "turn-1",
    );
    let regressed = decode(
        run("t", completed(40, 0, 2)).into_iter(),
        &mut threads,
        "turn-2",
    );
    assert_eq!(regressed.tokens, [0; 4]);
    assert!(regressed.tokens_partial);
    let missing = decode(
        run("t", json!({"type":"turn.completed"})).into_iter(),
        &mut threads,
        "turn-3",
    );
    assert_eq!(missing.tokens, [0; 4]);
    assert!(missing.tokens_partial);
    let next = decode(
        run("t", completed(130, 0, 13)).into_iter(),
        &mut threads,
        "turn-4",
    );
    assert_eq!(next.tokens, [30, 3, 0, 0]);
    assert!(next.prior_gap);
}
#[test]
fn a_stream_with_no_total_is_partial() {
    let mut threads = BTreeMap::new();
    let r = decode(vec![json!({})].into_iter(), &mut threads, "turn-1");
    assert!(r.tokens_partial && r.unpriced);
    let failed = decode(
        vec![
            json!({"type":"thread.started","thread_id":"t"}),
            json!({"type":"turn.started"}),
            json!({"type":"turn.failed","error":{"message":"x"}}),
        ]
        .into_iter(),
        &mut threads,
        "turn-2",
    );
    assert!(failed.tokens_partial);
}
#[test]
fn each_unreported_invocation_is_counted_once() {
    let started = || json!({"type":"turn.started"});
    let host = || json!({"type":"burrow.run","at":1});
    let failed = || json!({"type":"turn.failed","error":{"message":"x"}});
    let count = |records: Vec<Value>| {
        let mut threads = BTreeMap::new();
        decode(records.into_iter(), &mut threads, "t").missing.len()
    };
    // A failure is one gap, not another at EOF.
    assert_eq!(count(vec![host(), started(), failed()]), 1);
    // A retry dying before turn.started, after a completed run.
    let mut v = run("t", completed(1, 0, 1));
    v.push(host());
    assert_eq!(count(v), 1);
    // Failed, then a retry killed mid-turn: two runs, two gaps.
    assert_eq!(
        count(vec![host(), started(), failed(), host(), started()]),
        2
    );
    // The host marker and its turn.started are one invocation.
    assert_eq!(count(vec![host(), started()]), 1);
    // A run killed before its turn, then a retry that completes.
    let mut v = vec![host()];
    v.extend(run("t", completed(1, 0, 1)));
    assert_eq!(count(v), 1);
}
#[test]
fn an_incomplete_usage_object_is_no_total() {
    let mut threads = BTreeMap::new();
    decode(
        run("t", completed(100, 0, 10)).into_iter(),
        &mut threads,
        "turn-1",
    );
    // A killed run leaves a gap that an incomplete total must not recover.
    let killed = decode(
        vec![
            json!({"type":"burrow.run"}),
            json!({"type":"thread.started","thread_id":"t"}),
            json!({"type":"turn.started"}),
        ]
        .into_iter(),
        &mut threads,
        "turn-2",
    );
    for (i, usage) in [
        json!({}),
        json!({"input_tokens":500,"output_tokens":50}),
        json!({"input_tokens":"500","cached_input_tokens":0,"output_tokens":50}),
    ]
    .into_iter()
    .enumerate()
    {
        let r = decode(
            run("t", json!({"type":"turn.completed","usage":usage})).into_iter(),
            &mut threads,
            &format!("bad-{i}"),
        );
        assert_eq!(r.tokens, [0; 4]);
        assert!(r.tokens_partial && r.recovered.is_empty());
    }
    // Zero cache writes may be omitted; the baseline survived.
    let ok = decode(
        run(
            "t",
            json!({"type":"turn.completed","usage":{"input_tokens":130,"cached_input_tokens":0,"output_tokens":13}}),
        )
        .into_iter(),
        &mut threads,
        "turn-3",
    );
    assert_eq!(ok.tokens, [30, 3, 0, 0]);
    assert!(ok.recovered.is_superset(&killed.missing));
}
