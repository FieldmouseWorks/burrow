//! The machine out of Claude usage (proj's usage.rs): from 2026-11-01 to
//! 11-04 every Claude job was refused, and each wait on one went up the
//! ladder to the stuck issue, the cause named nowhere. Out of usage, a pass
//! holds what only a Claude can move instead (a turn in proj's line, a job
//! its source handed it to), hands it no job and books its next look for
//! the reset; after, each starts the ladder again from its kick. Anything
//! else overdue meanwhile is a failure of its own (a source stuck or never
//! written, a turn past its deadline): it's kicked, and with no Claude to
//! take it, neither a job of the watchdog's nor the stuck issue's, it goes
//! to the owner by the deadline a job's would have had.

mod common;

use burrow_watchdog::{GRACE, RENOTIFY, START};
use common::{H, Machine, T, item, named};
use serde_json::{Value, json};

/// The steward's PR waiting on a Claude job of its own (waits.py's claude).
fn on_claude(since: f64, due: f64) -> Value {
    let mut it = item(since, due, Some("autoland/sam/r"));
    it["claude"] = true.into();
    it
}

/// The owner's notices opened, by what they say.
fn opened(m: &Machine) -> Vec<String> {
    m.notices()
        .iter()
        .filter(|n| n[0] == "open")
        .map(|n| n[3].clone())
        .collect()
}

fn out_until(m: &Machine, until: f64) {
    *m.world.usage.borrow_mut() = json!({
        "until": until, "said": "You've hit your monthly spend limit", "job": "web-1", "at": T - H,
    })
    .to_string();
}

#[test]
fn out_of_usage_holds_what_waits_on_claude_till_it_resets() {
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
        json!({"pr-sam_r-7": on_claude(T - 3.0 * H, T - H)}),
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
        json!({"pr-sam_r-7": on_claude(T - 3.0 * H, T - H)}),
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
/// and holds what waits on Claude meanwhile; one proj won't take holds here, and is
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
            json!({"pr-sam_r-7": on_claude(T - 3.0 * H, T - H)}),
            T,
        );
        m.world.refuse.set(refuse);
        let (wake, stalls) = m.run(T);
        let holds = m
            .world
            .proj
            .borrow()
            .iter()
            .filter(|a| a[0] == "usage" && a.get(1).map(String::as_str) == Some("adopt"))
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
            .all(|a| a.get(1).map(String::as_str) != Some("adopt"))
    );
}

/// Out of usage, a steward that stopped writing its watch file, or never
/// wrote one, is no wait on Claude: it's kicked, no job is started for it
/// (one would only wait in line), and the owner hears by the deadline a
/// job's would have had, told why no Claude has it.
#[test]
fn out_of_usage_a_stuck_source_still_gets_its_kick_and_the_owner() {
    for written in [true, false] {
        let m = Machine::new();
        if written {
            m.write(&m.steward, json!({}), T - 2.0 * 86400.0);
        }
        m.write(&m.line, json!({}), T);
        let back = T + 3.0 * 86400.0;
        out_until(&m, back);
        // Never written: missing since this pass, and stale a day on.
        let start = if written { T } else { T + 86400.0 };
        if !written {
            m.pass(T, false);
            assert!(m.state().stalls.is_empty());
        }
        let (wake, stalls) = m.pass(start, false);
        assert_eq!(stalls.len(), 1, "{written} {stalls:?}");
        assert_eq!(m.kicks(), ["autoland"], "{written}");
        assert_eq!(wake, Some(start + GRACE), "{written}");
        let (wake, _) = m.pass(start + GRACE, false);
        assert!(m.runs().is_empty() && m.notices().is_empty(), "{written}");
        assert_eq!(wake, Some(start + m.cfg.escalate_after), "{written}");
        let (_, stalls) = m.pass(start + m.cfg.escalate_after, false);
        assert!(m.runs().is_empty() && m.escalated().is_empty(), "{written}");
        let said = opened(&m);
        assert_eq!(said.len(), 1, "{written} {said:?}");
        assert!(said[0].contains("Claude is out of usage until"), "{said:?}");
        assert!(stalls.values().all(|st| st.told.is_some()), "{written}");
    }
}

/// Out of usage, a turn at work past its deadline, and a PR whose wait
/// isn't on a Claude (its CI, say), are failures of their own: each has its
/// kick if it has one, no job, and the owner by its deadline, not the stuck
/// issue, whose job would only wait in line. Kept for the owner once usage
/// is back. A wait on Claude beside them is held all the while.
#[test]
fn out_of_usage_a_turn_past_its_deadline_and_other_waits_go_to_the_owner() {
    let m = Machine::new();
    m.write(
        &m.line,
        json!({"web-2": named("proj job web-2", "turn 1 is running", T - 3.0 * H, T - H)}),
        T,
    );
    let mut ci = item(T - 3.0 * H, T - H, Some("autoland/sam/r"));
    ci["claude"] = false.into();
    m.write(
        &m.steward,
        json!({"pr-sam_r-7": ci, "pr-sam_r-8": on_claude(T - 3.0 * H, T - H)}),
        T,
    );
    let back = T + 2.0 * 86400.0;
    out_until(&m, back);
    let (_, stalls) = m.run(T);
    assert_eq!(stalls.len(), 2, "{stalls:?}");
    assert_eq!(m.kicks(), ["autoland/sam/r"]);
    m.run(T + GRACE);
    assert!(m.runs().is_empty() && m.notices().is_empty());
    let (wake, stalls) = m.run(T + m.cfg.escalate_after);
    assert!(m.runs().is_empty() && m.escalated().is_empty());
    assert_eq!(opened(&m).len(), 2, "{:?}", m.notices());
    assert!(stalls.values().all(|st| st.told.is_some()), "{stalls:?}");
    // Held, its look booked for the reset.
    assert!(
        !stalls.keys().any(|s| s.contains("pr-sam_r-8")),
        "{stalls:?}"
    );
    assert!(wake.is_some_and(|w| w <= back), "{wake:?}");
    // Told again while it lasts, as any notice is.
    m.run(T + m.cfg.escalate_after + RENOTIFY);
    assert_eq!(opened(&m).len(), 4);
    // Back: the owner's they stay; the held one starts from its kick.
    *m.world.usage.borrow_mut() = String::new();
    let (_, stalls) = m.run(back);
    assert_eq!(stalls.len(), 3, "{stalls:?}");
    assert!(m.escalated().is_empty() && m.runs().is_empty());
    assert_eq!(m.kicks(), ["autoland/sam/r", "autoland/sam/r"]);
}

/// Out of usage, a stall on the stuck issue that isn't a wait on Claude
/// goes to the owner as well, since that issue's job can't take it; once
/// usage is back their notice closes and it's that job's again. One its own
/// watchdog job still waits in line for goes to the owner at the job's
/// deadline, the job kept in line as the only Claude still on it.
#[test]
fn out_of_usage_the_stuck_issue_and_a_queued_job_go_to_the_owner() {
    let m = Machine::new();
    m.write(
        &m.steward,
        json!({"pr-sam_r-7": item(T - 3.0 * H, T - H, Some("autoland/sam/r"))}),
        T,
    );
    m.run(T);
    m.run(T + GRACE);
    m.state_of("job-1", "running");
    let on = T + GRACE + m.cfg.escalate_after;
    m.run(on);
    assert_eq!(m.escalated().len(), 1);
    let back = on + 86400.0;
    out_until(&m, back);
    let (wake, stalls) = m.run(on + H);
    assert_eq!(m.escalated().len(), 1);
    let said = opened(&m);
    assert_eq!(said.len(), 1, "{:?}", m.notices());
    assert!(said[0].contains("nor the stuck issue's"), "{said:?}");
    assert!(stalls.values().all(|st| st.told.is_some()));
    assert!(wake.is_some_and(|w| w <= back));
    *m.world.usage.borrow_mut() = String::new();
    let (_, stalls) = m.run(back);
    assert!(
        m.notices().iter().any(|n| n[0] == "close"),
        "{:?}",
        m.notices()
    );
    assert!(
        stalls
            .values()
            .all(|st| st.told.is_none() && st.entry.is_some())
    );
    assert_eq!(m.escalated().len(), 1);

    let m = Machine::new();
    m.write(
        &m.steward,
        json!({"pr-sam_r-7": item(T - 3.0 * H, T - H, Some("autoland/sam/r"))}),
        T,
    );
    m.run(T);
    m.run(T + GRACE);
    m.state_of("job-1", "queued");
    out_until(&m, T + 5.0 * H);
    let (_, stalls) = m.run(T + 2.0 * GRACE);
    assert_eq!(stalls.len(), 1);
    assert!(m.withdrawals().is_empty() && m.notices().is_empty());
    m.run(T + GRACE + START);
    assert!(m.withdrawals().is_empty());
    assert_eq!(opened(&m).len(), 1);
    assert!(m.escalated().is_empty());
}
