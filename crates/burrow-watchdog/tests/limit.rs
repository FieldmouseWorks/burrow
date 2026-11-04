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
