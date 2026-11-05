//! The machine out of Claude usage (proj's usage.rs): from 2026-11-01 to
//! 11-04 every Claude job was refused, and each wait on one went up the
//! ladder to the stuck issue, the cause named nowhere. Out of usage, a pass
//! holds what's overdue instead, hands it no job and books its next look
//! for the reset; after, each starts the ladder again from its kick.

mod common;

use burrow_watchdog::GRACE;
use common::{H, Machine, T, item, named};
use serde_json::json;

fn out_until(m: &Machine, until: f64) {
    *m.world.usage.borrow_mut() = json!({
        "until": until, "said": "You've hit your monthly spend limit", "job": "web-1", "at": T - H,
    })
    .to_string();
}

#[test]
fn out_of_usage_holds_every_stall_till_it_resets() {
    let m = Machine::new();
    let mut line = named(
        "proj job web-1",
        "turn 1 is waiting in line for a slot",
        T - 3.0 * H,
        T - H,
    );
    line["line"] = true.into();
    m.write(&m.line, json!({"web-1": line}), T);
    m.write(
        &m.steward,
        json!({"pr-sam_r-7": item(T - 3.0 * H, T - H, Some("autoland/sam/r"))}),
        T,
    );
    let back = T + 2.0 * H;
    out_until(&m, back);
    let (wake, stalls) = m.run(T);
    assert!(stalls.is_empty(), "{stalls:?}");
    assert_eq!(wake, Some(back));
    assert!(m.kicks().is_empty() && m.runs().is_empty() && m.escalated().is_empty());
    assert!(m.notices().is_empty());
    // Still out an hour on: still held.
    let (_, stalls) = m.run(T + H);
    assert!(stalls.is_empty() && m.runs().is_empty() && m.escalated().is_empty());
    // Back: each from its first step, the steward's PR kicked, and proj's
    // turn in line, still overdue, on to the stuck issue as one is (no job
    // would get a slot; proj gives each a stall's time from the reset).
    *m.world.usage.borrow_mut() = String::new();
    let (_, stalls) = m.run(back);
    assert_eq!(stalls.len(), 2, "{stalls:?}");
    assert_eq!(m.kicks(), ["autoland/sam/r", "autoland"]);
    assert_eq!(m.escalated().len(), 1);
    assert!(m.runs().is_empty());
    let (_, _) = m.run(back + GRACE);
    assert_eq!(m.runs().len(), 1);
}

#[test]
fn a_stall_handed_on_before_stays_where_it_is_and_its_job_goes() {
    let m = Machine::new();
    m.write(
        &m.steward,
        json!({"pr-sam_r-7": item(T - 3.0 * H, T - H, Some("autoland/sam/r"))}),
        T,
    );
    m.run(T);
    m.run(T + GRACE);
    assert_eq!(m.runs().len(), 1);
    m.state_of("job-1", "queued");
    // Out of usage with the watchdog's own job still in line for it: held,
    // and that job withdrawn, since it would only be refused.
    out_until(&m, T + 5.0 * H);
    let (_, stalls) = m.run(T + 2.0 * GRACE);
    assert!(stalls.is_empty(), "{stalls:?}");
    assert_eq!(m.withdrawals(), ["job-1"]);
    // One the owner was told of before (the stuck issue's own job's) stays
    // theirs.
    let m = Machine::new();
    m.write(&m.line, json!({"homelab-9": common::unstick(true)}), T);
    m.write(&m.steward, json!({}), T);
    let (_, _) = m.run(T);
    assert_eq!(m.notices().len(), 1, "{:?}", m.notices());
    out_until(&m, T + 5.0 * H);
    let (_, stalls) = m.run(T + H);
    assert_eq!(stalls.len(), 1);
}

/// A hold #428's watchdog kept in its own state goes to proj's, machine-wide,
/// and holds every stall meanwhile; one proj won't take holds here, and is
/// handed over again next pass.
#[test]
fn a_hold_an_earlier_watchdog_kept_is_procs_now() {
    for refuse in [false, true] {
        let m = Machine::new();
        let until = T + 2.0 * H;
        std::fs::write(
            m.cfg.state_dir.join("state.json"),
            json!({"limit": until}).to_string(),
        )
        .unwrap();
        m.write(
            &m.steward,
            json!({"pr-sam_r-7": item(T - 3.0 * H, T - H, Some("autoland/sam/r"))}),
            T,
        );
        m.world.refuse.set(refuse);
        let (wake, stalls) = m.run(T);
        let holds = m
            .world
            .proj
            .borrow()
            .iter()
            .filter(|a| a[0] == "usage" && a.get(1).map(String::as_str) == Some("hold"))
            .count();
        assert_eq!(holds, 1, "{refuse}");
        assert!(
            stalls.is_empty() && m.kicks().is_empty() && m.runs().is_empty(),
            "{refuse}"
        );
        assert_eq!(wake, Some(until), "{refuse}");
        assert_eq!(m.state().limit, refuse.then_some(until), "{refuse}");
    }
    // One already past holds nothing, and goes.
    let m = Machine::new();
    std::fs::write(
        m.cfg.state_dir.join("state.json"),
        json!({"limit": T - 1.0}).to_string(),
    )
    .unwrap();
    m.run(T);
    assert_eq!(m.state().limit, None);
    assert!(
        m.world
            .proj
            .borrow()
            .iter()
            .all(|a| a.get(1).map(String::as_str) != Some("hold"))
    );
}
