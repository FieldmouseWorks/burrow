//! A usage limit one of its own jobs ended on (proj resets): it says nothing
//! of the stalls that job had, so none goes up the ladder for it, and no job
//! starts for any stall until the limit resets, when one takes them all.

mod common;

use burrow_watchdog::MIN;
use burrow_watchdog::watch::sid;
use common::{H, Machine, T, item};
use serde_json::json;

#[test]
fn waits_for_the_reset() {
    let m = Machine::new();
    m.write(
        &m.steward,
        json!({"sam/r#7": item(T - 3.0 * H, T - MIN, None)}),
        T,
    );
    m.run(T);
    assert_eq!(m.runs().len(), 1);
    // Its job ends on a usage limit that resets in two hours: the stall is
    // neither escalated nor handed another job, and the next look is then.
    m.limited("job-1", T + 2.0 * H);
    let (wake, stalls) = m.run(T + 10.0 * MIN);
    assert_eq!(wake, Some(T + 2.0 * H));
    assert!(m.escalated().is_empty(), "{:?}", m.escalated());
    assert_eq!(m.runs().len(), 1);
    assert_eq!(stalls[&sid("autoland", "sam/r#7", T - 3.0 * H)].job, None);
    assert_eq!(m.state().limit, Some(T + 2.0 * H));
    // One that stalls meanwhile waits with it.
    m.write(
        &m.steward,
        json!({"sam/r#7": item(T - 3.0 * H, T - MIN, None),
               "sam/r#8": item(T - 3.0 * H, T + 20.0 * MIN, None)}),
        T + 30.0 * MIN,
    );
    let (wake, _) = m.run(T + H);
    assert_eq!(wake, Some(T + 2.0 * H));
    assert!(m.escalated().is_empty() && m.runs().len() == 1);
    // Reset: one job takes both, and the limit is forgotten.
    let (_, stalls) = m.run(T + 2.0 * H);
    let runs = m.runs();
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert!(stalls.values().all(|st| st.job.is_some()), "{stalls:?}");
    assert!(m.escalated().is_empty());
    assert_eq!(m.state().limit, None);
}

#[test]
fn a_failed_start_before_it_is_forgotten() {
    let m = Machine::new();
    m.write(
        &m.steward,
        json!({"sam/r#7": item(T - 3.0 * H, T - MIN, None)}),
        T,
    );
    m.world.refuse.set(true);
    m.run(T);
    let stalls = m.state().stalls;
    assert!(stalls.values().all(|st| st.tried == Some(T)), "{stalls:?}");
    m.world.refuse.set(false);
    let (_, stalls) = m.run(T + 5.0 * MIN);
    let job = stalls.values().find_map(|st| st.job.clone()).unwrap();
    m.state_of(&job, "running");
    // Its job ends on a limit that resets past the stall's escalateAfter: the
    // start that failed at first takes it up the ladder neither while it
    // waits nor at the reset, when another job takes it.
    m.limited(&job, T + 3.0 * H);
    let (wake, _) = m.run(T + 10.0 * MIN);
    assert_eq!(wake, Some(T + 3.0 * H));
    m.run(T + 2.5 * H);
    assert!(m.escalated().is_empty(), "{:?}", m.escalated());
    let (_, stalls) = m.run(T + 3.0 * H);
    assert!(m.escalated().is_empty(), "{:?}", m.escalated());
    let now = stalls.values().find_map(|st| st.job.clone());
    assert!(now.is_some() && now != Some(job), "{stalls:?}");
}

#[test]
fn a_job_its_stall_left() {
    let m = Machine::new();
    m.write(
        &m.steward,
        json!({"sam/r#7": item(T - 3.0 * H, T - MIN, None)}),
        T,
    );
    let (_, stalls) = m.run(T);
    let job = stalls.values().find_map(|st| st.job.clone()).unwrap();
    m.state_of(&job, "running");
    // The stall moves again with its job still at work, which then ends on
    // a usage limit.
    m.write(&m.steward, json!({}), T + 10.0 * MIN);
    let (_, stalls) = m.run(T + 10.0 * MIN);
    assert!(stalls.is_empty(), "{stalls:?}");
    m.limited(&job, T + 2.0 * H);
    m.run(T + 20.0 * MIN);
    assert_eq!(m.state().limit, Some(T + 2.0 * H));
    // A stall from then on waits for the reset too.
    m.write(
        &m.steward,
        json!({"sam/r#8": item(T - 3.0 * H, T + 25.0 * MIN, None)}),
        T + 30.0 * MIN,
    );
    let (wake, _) = m.run(T + H);
    assert_eq!(wake, Some(T + 2.0 * H));
    assert_eq!(m.runs().len(), 1, "{:?}", m.runs());
    let (_, stalls) = m.run(T + 2.0 * H);
    assert_eq!(m.runs().len(), 2, "{:?}", m.runs());
    assert!(stalls.values().all(|st| st.job.is_some()), "{stalls:?}");
}
