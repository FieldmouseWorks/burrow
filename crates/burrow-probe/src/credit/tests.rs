use super::*;
use serde_json::json;
use std::fs;
use std::path::PathBuf;

const M: i64 = 1_000_000;
/// 2026-11-20T00:00:00Z.
const SINCE: i64 = 1_795_132_800;
const HOUR: i64 = 3600;

fn result(session: &str, total: f64) -> Value {
    json!({"type":"result","session_id":session,"usage":{"input_tokens":1,"output_tokens":1},"total_cost_usd":total})
}

fn init(session: &str) -> Value {
    json!({"type":"system","subtype":"init","session_id":session})
}

fn assistant(session: &str) -> Value {
    json!({"type":"assistant","session_id":session,"message":{"id":"m","usage":{"input_tokens":1}}})
}

/// The host's marker for a run of a turn, started at AT.
fn run(at: i64) -> Value {
    json!({"type":"burrow.run","at":at})
}

fn stream(records: &[Value]) -> String {
    records.iter().map(|r| format!("{r}\n")).collect()
}

struct Jobs {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Jobs {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("jobs");
        fs::create_dir_all(&root).unwrap();
        Jobs { _tmp: tmp, root }
    }

    /// Turn N of JOB on ENGINE (None: no engine file, so Claude), started
    /// at AT (None: no start recorded), with STREAM.
    fn turn(&self, job: &str, n: u64, engine: Option<&str>, at: Option<i64>, stream: &str) {
        let dir = self.root.join(job);
        fs::create_dir_all(&dir).unwrap();
        if let Some(engine) = engine {
            fs::write(dir.join(format!("{n}.engine")), format!("{engine}\n")).unwrap();
        }
        if let Some(at) = at {
            fs::write(dir.join(format!("{n}.started")), at.to_string()).unwrap();
        }
        fs::write(dir.join(format!("{n}.jsonl")), stream).unwrap();
    }

    /// The ledger's spends in order, each with its time; an error when one
    /// is pending.
    fn spends(&self, from: i64) -> Result<Vec<(i64, i64)>, String> {
        let ledger = ledger(&self.root, from)?;
        let mut spends = ledger.spends;
        spends.sort_by_key(|s| s.at);
        match ledger.pending {
            Some(pending) => Err(pending),
            None => Ok(spends.into_iter().map(|s| (s.at, s.micros)).collect()),
        }
    }

    /// The ledger's spends in order; an error when one is pending.
    fn ledger(&self, from: i64) -> Result<Vec<i64>, String> {
        let ledger = ledger(&self.root, from)?;
        let mut spends = ledger.spends;
        spends.sort_by_key(|s| s.at);
        match ledger.pending {
            Some(pending) => Err(pending),
            None => Ok(spends.into_iter().map(|s| s.micros).collect()),
        }
    }
}

#[test]
fn only_api_turns_of_the_cycle_count_and_a_session_carried_over_is_charged_its_own_part() {
    let jobs = Jobs::new();
    // The subscription ran $5 of session s; the API turn resumed it to $7.50.
    jobs.turn(
        "a",
        1,
        None,
        Some(SINCE + HOUR),
        &stream(&[result("s", 5.0)]),
    );
    jobs.turn(
        "a",
        2,
        Some(ENGINE),
        Some(SINCE + 2 * HOUR),
        &stream(&[result("s", 7.5)]),
    );
    // Before the cycle: the balance as set already had it.
    jobs.turn(
        "b",
        1,
        Some(ENGINE),
        Some(SINCE - 1),
        &stream(&[result("t", 40.0)]),
    );
    jobs.turn(
        "b",
        2,
        Some(ENGINE),
        Some(SINCE),
        &stream(&[result("t", 41.25)]),
    );
    // A job with no API turn isn't read at all, even when its stream isn't.
    jobs.turn("c", 1, Some("claude"), Some(SINCE), "not json\n");
    // Proj's own state and a file beside the jobs.
    fs::create_dir_all(jobs.root.join(".hidden")).unwrap();
    fs::write(jobs.root.join(".hidden/1.engine"), ENGINE).unwrap();
    fs::write(jobs.root.join("freeze.txt"), "x").unwrap();
    assert_eq!(jobs.ledger(SINCE).unwrap(), vec![1_250_000, 2_500_000]);
}

#[test]
fn retries_and_rereads_count_each_dollar_once() {
    let jobs = Jobs::new();
    // A turn retried in its own stream: two runs of one session, each
    // reporting the session's running total.
    let retried = stream(&[init("s"), result("s", 1.0), init("s"), result("s", 3.0)]);
    jobs.turn("a", 1, Some(ENGINE), Some(SINCE), &retried);
    // The next turn resumes it.
    jobs.turn(
        "a",
        2,
        Some(ENGINE),
        Some(SINCE + HOUR),
        &stream(&[result("s", 3.5)]),
    );
    let once = jobs.ledger(SINCE).unwrap();
    assert_eq!(once, vec![3 * M, M / 2]);
    assert_eq!(
        jobs.ledger(SINCE).unwrap(),
        once,
        "reading again adds nothing"
    );
}

#[test]
fn each_run_of_a_retried_turn_is_charged_at_its_own_start() {
    let jobs = Jobs::new();
    // A turn started late on the first day, retried after midnight: N.started
    // keeps the first run's time, each run's marker its own.
    let (first, retry) = (SINCE + 23 * HOUR, SINCE + 25 * HOUR);
    let retried = stream(&[
        run(first),
        init("s"),
        result("s", 1.0),
        run(retry),
        init("s"),
        result("s", 3.0),
    ]);
    jobs.turn("a", 1, Some(ENGINE), Some(first), &retried);
    assert_eq!(
        jobs.spends(SINCE).unwrap(),
        vec![(first, M), (retry, 2 * M)],
        "the retry is the next day's spend"
    );
    // A renewal or a new balance between them: only the retry is this cycle's.
    assert_eq!(
        jobs.spends(SINCE + 24 * HOUR).unwrap(),
        vec![(retry, 2 * M)]
    );
    // So today's spend is the retry's, against today's allowance.
    let credit = json!({"budgetUsd": 20, "renews": "2026-11-23", "monthlyUsd": 200, "reserveUsd": 5, "since": SINCE});
    let file = credit_file(jobs.root.parent().unwrap(), credit);
    let m = meter(&file, Some(&jobs.root), retry + HOUR).unwrap();
    assert_eq!((m.spent, m.today), (3 * M, 2 * M));

    // A retry still running after the cycle began holds the meter, though
    // the turn started before it.
    let running = Jobs::new();
    let stream = stream(&[
        run(first),
        init("s"),
        result("s", 1.0),
        run(retry),
        init("s"),
        assistant("s"),
    ]);
    running.turn("a", 1, Some(ENGINE), Some(first), &stream);
    let err = running.spends(SINCE + 24 * HOUR).unwrap_err();
    assert!(err.contains("no recorded cost yet"), "{err}");
}

#[test]
fn an_abandoned_run_is_charged_with_the_retry_that_accounts_for_it() {
    let jobs = Jobs::new();
    let (first, retry) = (SINCE + HOUR, SINCE + 2 * HOUR);
    // The first run stopped before its result; the retry resumed its session.
    let retried = stream(&[
        run(first),
        init("s"),
        assistant("s"),
        run(retry),
        init("s"),
        result("s", 2.0),
    ]);
    jobs.turn("a", 1, Some(ENGINE), Some(first), &retried);
    assert_eq!(
        jobs.spends(SINCE).unwrap(),
        vec![(first, 0), (retry, 2 * M)]
    );
    // Covered on the subscription instead: only the run that covers it is
    // charged, at its own start, not the whole turn.
    jobs.turn(
        "a",
        1,
        Some(ENGINE),
        Some(first),
        &stream(&[run(first), init("s"), assistant("s")]),
    );
    jobs.turn(
        "a",
        2,
        Some("claude"),
        Some(retry),
        &stream(&[
            run(retry),
            init("u"),
            result("u", 7.0),
            run(retry + HOUR),
            init("s"),
            result("s", 2.0),
        ]),
    );
    assert_eq!(
        jobs.spends(SINCE).unwrap(),
        vec![(first, 0), (retry + HOUR, 2 * M)]
    );
    // A marker without its time is an error, not a guess.
    jobs.turn(
        "b",
        1,
        Some(ENGINE),
        Some(first),
        &stream(&[json!({"type":"burrow.run"}), result("t", 1.0)]),
    );
    assert!(ledger(&jobs.root, SINCE).is_err());
}

#[test]
fn an_api_turn_without_its_cost_leaves_no_room_until_a_run_accounts_for_it() {
    let jobs = Jobs::new();
    // Running: its init and a message, no result yet, a line half written.
    let running = stream(&[init("s"), assistant("s")]) + r#"{"type":"assist"#;
    jobs.turn("a", 1, Some(ENGINE), Some(SINCE), &running);
    let err = jobs.ledger(SINCE).unwrap_err();
    assert!(err.contains("no recorded cost yet"), "{err}");
    // It stopped there; the next API turn resumes the session and reports
    // its total, the stopped run's included.
    fs::write(
        jobs.root.join("a/1.jsonl"),
        stream(&[init("s"), assistant("s")]),
    )
    .unwrap();
    jobs.turn(
        "a",
        2,
        Some(ENGINE),
        Some(SINCE + HOUR),
        &stream(&[result("s", 4.0)]),
    );
    assert_eq!(jobs.ledger(SINCE).unwrap(), vec![0, 4 * M]);
    // Resumed on the subscription instead: that turn is charged whole.
    fs::write(jobs.root.join("a/2.engine"), "claude").unwrap();
    assert_eq!(jobs.ledger(SINCE).unwrap(), vec![0, 4 * M]);
    // An earlier cycle's unfinished turn doesn't hold this one.
    assert_eq!(jobs.ledger(SINCE + 2 * HOUR).unwrap(), Vec::<i64>::new());
}

#[test]
fn unreadable_accounting_is_an_error() {
    let missing = Jobs::new();
    let gone = missing.root.join("nowhere");
    assert!(ledger(&gone, SINCE).is_err(), "no jobs folder");

    type Make = fn(&Jobs);
    let cases: [(&str, Make); 5] = [
        ("a malformed record", |j| {
            j.turn("a", 1, Some(ENGINE), Some(SINCE), "{\"type\":\"result\"\n");
        }),
        ("no start", |j| {
            j.turn("a", 1, Some(ENGINE), None, &stream(&[result("s", 1.0)]));
        }),
        ("a malformed start", |j| {
            j.turn(
                "a",
                1,
                Some(ENGINE),
                Some(SINCE),
                &stream(&[result("s", 1.0)]),
            );
            fs::write(j.root.join("a/1.started"), "soon").unwrap();
        }),
        ("a stream that isn't a file", |j| {
            j.turn("a", 1, Some(ENGINE), Some(SINCE), "");
            fs::remove_file(j.root.join("a/1.jsonl")).unwrap();
            fs::create_dir(j.root.join("a/1.jsonl")).unwrap();
        }),
        ("an engine that can't be read", |j| {
            j.turn(
                "a",
                1,
                Some(ENGINE),
                Some(SINCE),
                &stream(&[result("s", 1.0)]),
            );
            j.turn("a", 2, None, Some(SINCE), &stream(&[result("s", 2.0)]));
            fs::create_dir(j.root.join("a/2.engine")).unwrap();
        }),
    ];
    for (what, make) in cases {
        let jobs = Jobs::new();
        make(&jobs);
        assert!(ledger(&jobs.root, SINCE).is_err(), "{what}");
    }
    // A job folder that's a link isn't followed.
    let jobs = Jobs::new();
    let outside = jobs.root.parent().unwrap().join("outside");
    fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, jobs.root.join("linked")).unwrap();
    assert!(ledger(&jobs.root, SINCE).is_err());
}

fn credit_file(dir: &Path, credit: Value) -> PathBuf {
    let file = dir.join("overlay.credit.json");
    fs::write(&file, json!({"credit": credit}).to_string()).unwrap();
    file
}

#[test]
fn the_meter_refuses_a_turn_once_the_reserve_is_reached() {
    let jobs = Jobs::new();
    let credit = json!({"budgetUsd": 20, "renews": "2026-11-21", "monthlyUsd": 200, "reserveUsd": 5, "since": SINCE});
    let file = credit_file(jobs.root.parent().unwrap(), credit);
    let now = SINCE + 20 * HOUR; // the last day
    let m = meter(&file, Some(&jobs.root), now).unwrap();
    assert!(m.room());
    assert_eq!(
        (m.cycle.budget, m.allowance, m.days_left),
        (20 * M, 20 * M, 1)
    );
    jobs.turn(
        "a",
        1,
        Some(ENGINE),
        Some(SINCE + HOUR),
        &stream(&[result("s", 14.99)]),
    );
    assert!(meter(&file, Some(&jobs.root), now).unwrap().room());
    jobs.turn(
        "a",
        2,
        Some(ENGINE),
        Some(SINCE + 2 * HOUR),
        &stream(&[result("s", 15.0)]),
    );
    let m = meter(&file, Some(&jobs.root), now).unwrap();
    assert_eq!(m.spent, 15 * M);
    assert!(!m.room(), "$15 spent + $5 reserve of $20 is no room");
    // A turn still running: no room, and the meter still shows its numbers.
    let pending = Jobs::new();
    let started = stream(&[result("p", 2.0), init("p"), assistant("p")]);
    pending.turn("a", 1, Some(ENGINE), Some(SINCE + HOUR), &started);
    let m = meter(&file, Some(&pending.root), now).unwrap();
    assert_eq!(m.spent, 2 * M);
    assert!(!m.room());
    let text = m.text();
    assert!(text.contains("no recorded cost yet"), "{text}");
    assert!(text.contains("spent $2.00"), "{text}");
}

#[test]
fn no_setting_or_no_jobs_is_no_meter() {
    let jobs = Jobs::new();
    let dir = jobs.root.parent().unwrap();
    let err = meter(&dir.join("absent.json"), Some(&jobs.root), SINCE).unwrap_err();
    assert_eq!(err, "no API credit is set");
    fs::write(dir.join("off.json"), r#"{"credit":null}"#).unwrap();
    assert!(meter(&dir.join("off.json"), Some(&jobs.root), SINCE).is_err());
    fs::write(dir.join("bad.json"), r#"{"credit":{"budgetUsd":1}}"#).unwrap();
    assert!(meter(&dir.join("bad.json"), Some(&jobs.root), SINCE).is_err());
    let credit = json!({"budgetUsd": 20, "renews": "2026-11-21", "monthlyUsd": 200, "reserveUsd": 5, "since": SINCE});
    let file = credit_file(dir, credit);
    assert!(meter(&file, None, SINCE).is_err());
    assert!(meter(&file, Some(&jobs.root), SINCE).unwrap().room());
}

/// The host's marker for a run admitted with BUDGET micro-dollars.
fn budgeted(at: i64, budget: i64) -> Value {
    json!({"type":"burrow.run","at":at,"budget":budget})
}

/// A run still going is held at the budget its admission gave it, so runs
/// admitted at once never share the same room; its recorded cost replaces
/// that once it's in, and a later run of its session that accounts for it
/// releases it. A turn with nothing in its stream yet started no run.
#[test]
fn a_run_without_its_cost_is_held_at_its_budget() {
    let jobs = Jobs::new();
    let at = SINCE + HOUR;
    // Going now, admitted with $3, nothing spent yet that it has said.
    jobs.turn(
        "a",
        1,
        Some(ENGINE),
        Some(at),
        &stream(&[budgeted(at, 3 * M), init("s"), assistant("s")]),
    );
    // Launching: its turn's files are written, its run not yet.
    jobs.turn("b", 1, Some(ENGINE), None, "");
    assert_eq!(jobs.spends(SINCE).unwrap(), [(at, 3 * M)]);
    // Its result: what it spent, not what it was allowed.
    jobs.turn(
        "a",
        1,
        Some(ENGINE),
        Some(at),
        &stream(&[budgeted(at, 3 * M), init("s"), result("s", 1.25)]),
    );
    assert_eq!(jobs.spends(SINCE).unwrap(), [(at, 1_250_000)]);
    // Stopped before its result: still held at its budget, never pending,
    // until the session's next run says what it all cost.
    jobs.turn(
        "c",
        1,
        Some(ENGINE),
        Some(at),
        &stream(&[budgeted(at, 2 * M), init("t"), assistant("t")]),
    );
    let mut spends = jobs.spends(SINCE).unwrap();
    spends.sort();
    assert_eq!(spends, [(at, 1_250_000), (at, 2 * M)]);
    jobs.turn(
        "c",
        2,
        Some(ENGINE),
        Some(at + 1),
        &stream(&[budgeted(at + 1, 2 * M), init("t"), result("t", 1.5)]),
    );
    let mut spends = jobs.spends(SINCE).unwrap();
    spends.sort();
    assert_eq!(spends, [(at, 0), (at, 1_250_000), (at + 1, 1_500_000)]);
    // A run with no budget in its marker (from before admission wrote one)
    // still can't be known: pending, no room.
    jobs.turn(
        "d",
        1,
        Some(ENGINE),
        Some(at),
        &stream(&[run(at), init("u")]),
    );
    assert!(
        jobs.spends(SINCE)
            .unwrap_err()
            .contains("no recorded cost yet")
    );
}

/// The host's marker for a run on ENGINE, with BUDGET when it has one.
fn on(at: i64, engine: &str, budget: Option<i64>) -> Value {
    match budget {
        Some(b) => json!({"type":"burrow.run","at":at,"engine":engine,"budget":b}),
        None => json!({"type":"burrow.run","at":at,"engine":engine}),
    }
}

/// A turn its route moved between the subscription and the credit, in one
/// session, is charged for its credit runs alone, each by the engine its
/// marker names: a subscription run refused before its cost is neither
/// charged nor pending, and a subscription run after the credit's isn't
/// charged either. Its turn's engine (the credit, once it's been on it)
/// still finds it.
#[test]
fn a_moved_turn_is_charged_for_its_credit_runs_alone() {
    let jobs = Jobs::new();
    let (first, second) = (SINCE + HOUR, SINCE + 2 * HOUR);
    let refused = json!({"type":"result","session_id":"s","is_error":true,"result":"Claude reached its weekly allowance"});
    // Out of usage, then on to the credit.
    jobs.turn(
        "onto",
        1,
        Some(ENGINE),
        Some(first),
        &stream(&[
            on(first, "claude", None),
            init("s"),
            refused,
            on(second, ENGINE, Some(5 * M)),
            init("s"),
            result("s", 2.0),
        ]),
    );
    assert_eq!(jobs.spends(SINCE).unwrap(), [(second, 2 * M)]);
    // The credit short, then back on the subscription: its $1.50 alone.
    let jobs = Jobs::new();
    jobs.turn(
        "back",
        1,
        Some(ENGINE),
        Some(first),
        &stream(&[
            on(first, ENGINE, Some(3 * M)),
            init("t"),
            result("t", 1.5),
            on(second, "claude", None),
            init("t"),
            result("t", 4.0),
        ]),
    );
    assert_eq!(jobs.spends(SINCE).unwrap(), [(first, 1_500_000)]);
}

/// A turn from before markers named their engine (a usage refusal held on
/// the subscription across the deploy, say) that its route then moves onto
/// the credit: proj keeps the engine it ran on in N.engine-before, so its
/// older runs stay the subscription's, neither charged nor pending, and
/// only its credit run is charged. Without that record they fall back to
/// the turn's engine, which names the credit.
#[test]
fn a_turn_moved_onto_the_credit_keeps_its_older_runs_on_the_subscription() {
    let (first, second, third) = (SINCE + HOUR, SINCE + 2 * HOUR, SINCE + 3 * HOUR);
    let refused = json!({"type":"result","session_id":"s","is_error":true,"result":"Claude reached its weekly allowance"});
    let jobs = Jobs::new();
    jobs.turn(
        "held",
        1,
        Some(ENGINE),
        Some(first),
        &stream(&[
            run(first),
            init("s"),
            result("s", 1.0),
            run(second),
            init("s"),
            refused,
            on(third, ENGINE, Some(5 * M)),
            init("s"),
            result("s", 3.0),
        ]),
    );
    fs::write(
        jobs.root.join(format!("held/1.{}", credit::ENGINE_BEFORE)),
        "claude\n",
    )
    .unwrap();
    assert_eq!(jobs.spends(SINCE).unwrap(), [(third, 2 * M)]);
    // Without it, the older runs are the credit's: the subscription's
    // dollar is charged to it.
    fs::remove_file(jobs.root.join(format!("held/1.{}", credit::ENGINE_BEFORE))).unwrap();
    assert_eq!(
        jobs.spends(SINCE).unwrap(),
        [(first, M), (second, 0), (third, 2 * M)]
    );
    // A turn that was on the credit before it moved needs no record: its
    // older runs are its turn's, the credit's.
    let jobs = Jobs::new();
    jobs.turn(
        "api",
        1,
        Some(ENGINE),
        Some(first),
        &stream(&[
            run(first),
            init("t"),
            result("t", 1.0),
            on(second, ENGINE, Some(5 * M)),
            init("t"),
            result("t", 3.0),
        ]),
    );
    assert_eq!(jobs.spends(SINCE).unwrap(), [(first, M), (second, 2 * M)]);
}
