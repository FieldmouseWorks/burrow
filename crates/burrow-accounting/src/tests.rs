use super::*;
use serde_json::json;
use std::io::{Seek, SeekFrom, Write};

fn result(session: &str, input: u64, cost: f64) -> Value {
    json!({"type":"result","session_id":session,"usage":{"input_tokens":input,"output_tokens":3,"cache_read_input_tokens":2,"cache_creation_input_tokens":1},"total_cost_usd":cost})
}
fn call(session: &str, id: &str, input: u64, read: u64, written: u64) -> Value {
    json!({"type":"assistant","session_id":session,"parent_tool_use_id":null,"message":{"id":id,"usage":{"input_tokens":input,"output_tokens":1,"cache_read_input_tokens":read,"cache_creation_input_tokens":written}}})
}
fn crash(session: &str) -> Value {
    json!({"type":"result","subtype":"error_during_execution","session_id":session,"usage":{"input_tokens":0},"total_cost_usd":0})
}
fn run() -> Value {
    json!({"type":"burrow.run","at":1})
}
fn codex(thread: &str, input: u64, cached: u64, output: u64) -> Vec<Value> {
    vec![
        run(),
        json!({"type":"thread.started","thread_id":thread}),
        json!({"type":"turn.started"}),
        json!({"type":"turn.completed","usage":{"input_tokens":input,"cached_input_tokens":cached,"cache_write_input_tokens":0,"output_tokens":output}}),
    ]
}

/// One job's turns: engine, start, stream. Sessions resumed and fresh,
/// an engine change each way, an appended retry repeating its session's
/// cumulative cost, a crash recovered later, and a regressed counter.
fn history() -> Vec<(&'static str, i64, Vec<Value>)> {
    vec![
        // Session a: 1.0 so far.
        ("claude", 100, vec![run(), result("a", 10, 1.0)]),
        // A crash, then an appended retry whose total repeats a's growth:
        // the retry recovers the crashed run.
        (
            "claude",
            200,
            vec![
                run(),
                crash("a"),
                run(),
                result("a", 20, 1.5),
                result("a", 5, 1.5),
            ],
        ),
        // A fresh session after a handoff.
        ("claude", 300, vec![run(), result("b", 30, 0.25)]),
        // Codex, a thread of its own, then resumed.
        ("codex", 400, codex("t", 100, 60, 10)),
        ("codex", 500, codex("t", 250, 150, 25)),
        // Back to Claude, resuming b; a regressed counter in between is
        // missing and never replaces b's last good total.
        (
            "claude",
            600,
            vec![run(), result("b", 7, 0.1), run(), result("b", 8, 0.75)],
        ),
    ]
}

fn replay(prices: Prices) -> Job {
    let mut job = Job::new(prices);
    for (n, (engine, at, records)) in history().into_iter().enumerate() {
        job.begin(Meta {
            identity: format!("job/{}", n + 1),
            engine: engine.into(),
            model: None,
            started: Some(at),
        });
        for v in &records {
            job.feed(v);
        }
    }
    job.end();
    job
}

#[test]
fn lifetime_totals_reconcile_with_their_turns_without_double_counting() {
    let job = replay(Prices::new());
    let turns = job.turns();
    let costs: Vec<_> = turns
        .iter()
        .map(|t| t.report.as_ref().unwrap().cost)
        .collect();
    assert_eq!(
        costs,
        [Some(1.0), Some(0.5), Some(0.25), None, None, Some(0.5)]
    );
    let totals = job.totals();
    // Each session's last trustworthy cumulative total, once: a's 1.5 and
    // b's 0.75, however often either was repeated or resumed.
    assert_eq!(totals.cost, 1.5 + 0.75);
    // Claude tokens are per invocation; Codex's thread t counts its last
    // total once.
    assert_eq!(
        totals.tokens,
        [
            10 + 20 + 5 + 30 + 7 + 8 + (250 - 150),
            3 * 6 + 25,
            2 * 6 + 150,
            6
        ]
    );
    let mut sum = Tally::default();
    for t in &turns {
        sum.add_turn(t);
    }
    assert_eq!(sum, totals);
    assert_eq!(
        totals.sessions,
        BTreeSet::from(["a", "b", "t"].map(String::from))
    );
    assert_eq!((totals.turns, totals.measured, totals.unpriced), (6, 4, 2));
    // The crash was recovered by its own turn's retry, the regressed
    // counter's run by b's next total: the cost is complete.
    assert_eq!(totals.recovered_here(), 2);
    assert_eq!(totals.still_missing(), 0);
    assert_eq!(totals.money(), "cost=$2.25 2 turns cost unavailable");
}

#[test]
fn a_span_and_the_rest_add_up_to_the_lifetime() {
    let job = replay(Prices::new());
    let lifetime = job.totals();
    for cut in [150, 350, 450, 550] {
        let (mut early, mut late) = (Tally::default(), Tally::default());
        for t in job.turns() {
            if t.meta.started.unwrap() < cut {
                early.add_turn(&t);
            } else {
                late.add_turn(&t);
            }
        }
        // Resumes counted only what they added, so a span never takes in
        // the cost of turns before it.
        let mut both = early.clone();
        both.add(&late);
        assert_eq!(both.tokens, lifetime.tokens, "cut at {cut}");
        assert!((both.cost - lifetime.cost).abs() < 1e-12, "cut at {cut}");
        assert_eq!(both.turns, lifetime.turns);
    }
}

#[test]
fn incremental_replay_agrees_with_complete_replay() {
    let complete = replay(Prices::new()).turns();
    // Each turn's stream written a few bytes at a time, read as it grows,
    // with a snapshot after every read.
    let dir = tempfile::tempdir().unwrap();
    let mut job = Job::new(Prices::new());
    for (n, (engine, at, records)) in history().into_iter().enumerate() {
        let path = dir.path().join(format!("{}.jsonl", n + 1));
        let mut writer = std::fs::File::create(&path).unwrap();
        let mut reader = std::fs::File::open(&path).unwrap();
        let bytes: Vec<u8> = records
            .iter()
            .flat_map(|v| format!("{v}\n").into_bytes())
            .collect();
        job.begin(Meta {
            identity: format!("job/{}", n + 1),
            engine: engine.into(),
            model: None,
            started: Some(at),
        });
        let mut tail = Tail::default();
        let mut previous = None;
        for chunk in bytes.chunks(37) {
            writer.write_all(chunk).unwrap();
            for v in tail.read(&mut reader, false).unwrap() {
                job.feed(&v);
            }
            let snap = job.snapshot().unwrap();
            assert!(snap.report.as_ref().unwrap().unfinished);
            // A snapshot changes nothing: taking it twice gives the same.
            assert_eq!(Some(&snap), job.snapshot().as_ref());
            previous = Some(snap);
        }
        assert!(tail.read(&mut reader, true).unwrap().is_empty());
        assert_eq!(tail.offset, bytes.len() as u64);
        // The last snapshot is the complete turn, but for being unfinished.
        let mut last = previous.unwrap();
        last.report.as_mut().unwrap().unfinished = false;
        assert_eq!(last, complete[n], "turn {}", n + 1);
    }
    job.end();
    assert_eq!(job.turns(), complete);
}

#[test]
fn a_half_written_line_waits_and_an_ended_stream_takes_its_last_line() {
    let mut stream = std::io::Cursor::new(Vec::new());
    let mut tail = Tail::default();
    stream.write_all(b"{\"type\":\"a\"}\n{\"type\":").unwrap();
    let got = tail.read(&mut stream, false).unwrap();
    assert_eq!(got, [json!({"type":"a"})]);
    assert_eq!(tail.offset, 13);
    stream.seek(SeekFrom::End(0)).unwrap();
    stream.write_all(b"\"b\"}").unwrap();
    assert!(tail.read(&mut stream, false).unwrap().is_empty());
    assert_eq!(tail.read(&mut stream, true).unwrap(), [json!({"type":"b"})]);
}

#[test]
fn a_snapshot_of_an_unfinished_run_counts_what_its_end_would() {
    let mut job = Job::new(Prices::new());
    job.begin(Meta {
        identity: "job/1".into(),
        engine: "claude".into(),
        ..Meta::default()
    });
    for v in [result("a", 10, 1.0), run(), call("a", "m", 4, 100, 6)] {
        job.feed(&v);
    }
    let snap = job.snapshot().unwrap().report.unwrap();
    assert!(snap.unfinished && snap.cost_partial && snap.tokens_partial);
    assert_eq!(snap.cost, Some(1.0));
    // The open run's output is a placeholder until its result.
    assert_eq!(snap.tokens, [14, 3, 102, 7]);
    assert!(
        snap.show(None)
            .ends_with("unfinished (no outcome yet: counts to its last complete record)")
    );
    job.end();
    let ended = job.turns().pop().unwrap().report.unwrap();
    assert_eq!(
        Report {
            unfinished: true,
            ..ended
        },
        snap
    );
    // The next turn's result recovers the open run's cost.
    job.begin(Meta {
        identity: "job/2".into(),
        engine: "claude".into(),
        ..Meta::default()
    });
    job.feed(&result("a", 1, 3.0));
    job.end();
    let totals = job.totals();
    assert_eq!(totals.cost, 3.0);
    assert_eq!((totals.still_missing(), totals.recovered_here()), (0, 1));
    assert_eq!(totals.money(), "cost=$3.00");
}

#[test]
fn context_is_the_latest_prompt_not_the_traffic() {
    let mut sessions = BTreeMap::new();
    let records = vec![
        run(),
        call("a", "m1", 10, 0, 1000),
        // The same call again, as a later content block with fewer fields.
        json!({"type":"assistant","session_id":"a","parent_tool_use_id":null,"message":{"id":"m1","usage":{"output_tokens":9}}}),
        call("a", "m2", 5, 1000, 200),
        // A subagent's call: its own, smaller context.
        json!({"type":"assistant","session_id":"a","parent_tool_use_id":"tool","message":{"id":"s1","usage":{"input_tokens":3,"cache_read_input_tokens":50}}}),
        call("a", "m3", 2, 1200, 30),
        json!({"type":"result","session_id":"a","usage":{"input_tokens":20,"output_tokens":40,"cache_read_input_tokens":2250,"cache_creation_input_tokens":1230},"total_cost_usd":0.5}),
    ];
    let r = claude::decode(records, &mut sessions, "job/1");
    assert_eq!(r.context, Some(2 + 1200 + 30));
    assert_eq!(r.context_peak, Some(2 + 1200 + 30));
    // Traffic is every call's prompt summed: far more than the context.
    assert_eq!(r.tokens, [20, 40, 2250, 1230]);
    assert!(r.tokens[0] + r.tokens[2] + r.tokens[3] > 2 * r.context.unwrap());
    assert!(
        r.show(None)
            .contains("cost=$0.500000 context=1232 peak=1232"),
        "{}",
        r.show(None)
    );
    // A fresh session's context starts small again; the peak stays.
    let mut job = Job::new(Prices::new());
    for (n, records) in [
        vec![call("a", "x", 1, 5000, 0), result("a", 1, 0.1)],
        vec![call("b", "y", 1, 0, 300), result("b", 1, 0.1)],
        codex("t", 10, 0, 1),
    ]
    .into_iter()
    .enumerate()
    {
        job.begin(Meta {
            identity: format!("job/{n}"),
            engine: if n == 2 { "codex" } else { "claude" }.into(),
            ..Meta::default()
        });
        for v in &records {
            job.feed(v);
        }
        let t = job.totals();
        match n {
            0 => assert_eq!((t.context, t.context_peak), (Some(5001), Some(5001))),
            1 => assert_eq!((t.context, t.context_peak), (Some(301), Some(5001))),
            // Codex's stream doesn't say: unavailable, not the last Claude's.
            _ => assert_eq!((t.context, t.context_peak), (None, Some(5001))),
        }
    }
}

#[test]
fn estimates_are_configured_labelled_and_never_replace_measured_cost() {
    let mut prices = Prices::new();
    let price = |n: f64| Price {
        input: n,
        output: 10.0 * n,
        cache_read: n / 10.0,
        cache_write: 0.0,
    };
    prices.insert("codex".into(), price(1.0));
    prices.insert("codex/special".into(), price(2.0));
    // A Claude price is configured too, but Claude's cost is measured.
    prices.insert("claude".into(), price(100.0));
    let job = replay(prices.clone());
    let turns = job.turns();
    assert!(
        turns
            .iter()
            .filter(|t| t.meta.engine == "claude")
            .all(|t| t.estimate.is_none())
    );
    let estimates: Vec<_> = turns.iter().filter_map(|t| t.estimate.clone()).collect();
    // Thread t's turns: 40 input, 60 cached, 10 output; then 60, 90, 15.
    let expect = [(40.0 + 100.0 + 6.0) / 1e6, (60.0 + 150.0 + 9.0) / 1e6];
    assert_eq!(estimates.len(), 2);
    for (e, want) in estimates.iter().zip(expect) {
        assert!((e.usd - want).abs() < 1e-15, "{e:?}");
        assert_eq!(e.basis, "codex");
    }
    let totals = job.totals();
    assert_eq!(totals.cost, 2.25);
    assert_eq!((totals.estimated, totals.unpriced), (2, 0));
    assert_eq!(totals.money(), "cost=$2.25 estimate=$0.00 (codex)");
    let line = turns[3]
        .report
        .as_ref()
        .unwrap()
        .show(turns[3].estimate.as_ref());
    assert!(
        line.contains(" estimate=$0.000146 (codex) context unavailable"),
        "{line}"
    );
    // A model's own key wins over its engine's.
    let mut special = Job::new(prices.clone());
    special.begin(Meta {
        identity: "job/1".into(),
        engine: "codex".into(),
        model: Some("special".into()),
        started: None,
    });
    for v in codex("u", 1_000_000, 0, 0) {
        special.feed(&v);
    }
    let e = special.snapshot().unwrap().estimate.unwrap();
    assert_eq!((e.usd, e.basis.as_str()), (2.0, "codex/special"));
    // An engine with no price stays unavailable, and an unread one too.
    let mut bare = Job::new(Prices::new());
    for engine in ["codex", "other"] {
        bare.begin(Meta {
            identity: format!("job/{engine}"),
            engine: engine.into(),
            ..Meta::default()
        });
    }
    bare.end();
    let t = bare.totals();
    assert_eq!((t.unpriced, t.unread), (1, 1));
    assert_eq!(t.money(), "2 turns cost unavailable");
    assert!(
        bare.turns()[0]
            .report
            .as_ref()
            .unwrap()
            .show(None)
            .contains("cost unpriced")
    );
    assert!(describe_prices(&Prices::new()).starts_with("no prices configured"));
    assert!(
        describe_prices(&prices).contains("codex 1/10/0.1/0; codex/special 2/20/0.2/0"),
        "{}",
        describe_prices(&prices)
    );
}
