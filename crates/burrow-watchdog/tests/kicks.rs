//! What a kick costs: one pass for all that share it, and no more however
//! often the run it started takes the wait up and fails it again.

mod common;

use burrow_watchdog::watch::sid;
use burrow_watchdog::{GRACE, MIN};
use common::{H, Machine, T, named};
use serde_json::{Value, json};

const DEPLOYS: [&str; 2] = ["deploy/sam/r", "deploy/sam/q"];

/// Both repos' waits on box-update, as burrow-dispatch writes them.
fn waits(since: f64) -> Value {
    let mut it = named(
        "main going live",
        "2 commits; box-update's last run: failed: the VM test fails with them",
        since,
        since + 30.0 * MIN,
    );
    it["kick"] = "update".into();
    it["refresh"] = false.into();
    DEPLOYS.map(|key| (key, it.clone())).into_iter().collect()
}

/// The same waits while a run that started at `start` has them.
fn taken(start: f64) -> Value {
    let mut it = named(
        "main going live",
        "box-update has been putting it live since then",
        start,
        start + 2.0 * H,
    );
    it["kick"] = "dispatch".into();
    it["refresh"] = true.into();
    DEPLOYS.map(|key| (key, it.clone())).into_iter().collect()
}

// Two waits past their deadline on one handler are one kick: a second lands
// after the first's pass has started, and is another run straight after it.
#[test]
fn one_kick_a_handler_a_pass() {
    let m = Machine::new();
    m.write(&m.steward, waits(T - 2.0 * H), T);
    let (wake, stalls) = m.run(T);
    assert_eq!(m.kicks(), ["update"]);
    assert_eq!(wake, Some(T + GRACE));
    assert!(stalls.values().all(|st| st.kicked == Some(T)));
}

// The run a kick started takes the wait up (not overdue meanwhile) and
// fails: the same stall, already kicked, so no kick again, however many
// runs do, and one job once the kick has had its time, which a later run
// taking the wait up neither forgets nor withdraws.
#[test]
fn a_failed_run_is_not_kicked_again() {
    let m = Machine::new();
    let since = T - 2.0 * H;
    m.write(&m.steward, waits(since), T);
    m.run(T);
    let mut at = T;
    for _ in 0..2 {
        m.write(&m.steward, taken(at + 10.0), at + 20.0);
        assert_eq!(m.run(at + 20.0).1.len(), 2, "the run forgot the stalls");
        m.write(&m.steward, waits(since), at + MIN);
        m.run(at + MIN);
        assert_eq!(m.kicks(), ["update"]);
        at += MIN;
    }
    assert!(m.runs().is_empty());
    m.run(T + GRACE);
    assert_eq!(m.runs().len(), 1);
    m.state_of("job-1", "queued");
    m.write(&m.steward, taken(T + GRACE + MIN), T + GRACE + 2.0 * MIN);
    m.run(T + GRACE + 2.0 * MIN);
    assert!(m.withdrawals().is_empty());
    m.write(&m.steward, waits(since), T + GRACE + 3.0 * MIN);
    m.run(T + GRACE + 3.0 * MIN);
    assert_eq!((m.kicks().len(), m.runs().len()), (1, 1));
}

// What's on the stuck issue stays on it while a run looks: off and back
// would be a turn of that issue's job each run.
#[test]
fn a_run_leaves_the_stuck_issue_be() {
    let m = Machine::new();
    let since = T - 2.0 * H;
    m.write(&m.steward, waits(since), T);
    m.run(T);
    m.run(T + GRACE);
    m.run(T + GRACE + MIN);
    m.run(T + 2.0 * GRACE + MIN);
    let entries = m.escalated();
    assert!(!entries.is_empty());
    let kicks = m.kicks();
    let at = T + 3.0 * GRACE;
    m.write(&m.steward, taken(at), at);
    m.run(at);
    assert_eq!((m.escalated(), m.kicks()), (entries, kicks));
}

// A run that ends the wait clears its stall; a commit after that is a wait
// of its own, with its own kick.
#[test]
fn a_run_that_goes_through_clears_it() {
    let m = Machine::new();
    let since = T - 2.0 * H;
    m.write(&m.steward, waits(since), T);
    m.run(T);
    m.write(&m.steward, taken(T + 10.0), T + MIN);
    m.run(T + MIN);
    m.write(&m.steward, json!({}), T + 2.0 * MIN);
    assert!(m.run(T + 2.0 * MIN).1.is_empty());
    // And one whose oldest commit moved on, with no moment of none waiting.
    m.write(&m.steward, waits(since), T + 3.0 * MIN);
    m.run(T + 3.0 * MIN);
    m.write(&m.steward, waits(since + H), T + 4.0 * MIN);
    let (_, stalls) = m.run(T + 4.0 * MIN);
    let want = DEPLOYS.map(|key| sid("autoland", key, since + H));
    assert!(stalls.len() == 2 && want.iter().all(|s| stalls.contains_key(s)));
    assert_eq!(m.kicks(), ["update", "update", "update"]);
}
