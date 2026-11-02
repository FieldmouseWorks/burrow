//! What a run remembers, on a real clone (testing.rs).

use std::fs;

use super::*;
use crate::testing::Machine;

fn memo(m: &Machine) -> Memo<'_> {
    Memo {
        cfg: &m.cfg,
        force: false,
    }
}

fn tree(m: &Machine, commit: &str) -> String {
    m.work(&["rev-parse", &format!("{commit}^{{tree}}")])
}

#[test]
fn a_tree_that_failed_to_build_is_remembered_for_a_day() {
    let m = Machine::new();
    let memo = memo(&m);
    assert!(!memo.failed_before("build"));
    memo.failed("build").unwrap();
    assert!(memo.failed_before("build"));
    assert!(!memo.failed_before("check"), "another step's failure");
    let failed = tree(&m, "HEAD");
    // Another tree hasn't failed; the same tree from another commit has.
    m.commit("flake.lock", "v2");
    assert!(!memo.failed_before("build"));
    m.commit("flake.lock", "v1");
    assert!(memo.failed_before("build"));
    // A day on, it's tried again; forced, at once.
    let old = epoch() - DAY;
    fs::write(
        m.cfg.state.join("failed-trees"),
        format!("build:{failed} {old}\n"),
    )
    .unwrap();
    assert!(!memo.failed_before("build"));
    memo.failed("build").unwrap();
    let forced = Memo {
        cfg: &m.cfg,
        force: true,
    };
    assert!(!forced.failed_before("build"));
}

#[test]
fn a_vm_tests_last_result_on_a_tree_is_remembered_for_a_day() {
    let m = Machine::new();
    let memo = memo(&m);
    let a = m.head();
    assert_eq!(memo.result_of("vm", &a), None);
    memo.record("vm", &a, Verdict::Fail).unwrap();
    assert_eq!(memo.result_of("vm", &a), Some(Verdict::Fail));
    memo.record("vm", &a, Verdict::Pass).unwrap();
    assert_eq!(memo.result_of("vm", &a), Some(Verdict::Pass), "the last");
    assert_eq!(memo.result_of("other", &a), None, "another test");
    m.commit("flake.lock", "v2");
    let b = m.head();
    assert_eq!(memo.result_of("vm", &b), None, "another tree");
    // What's a day old is gone, and a result it can't read is none.
    let (old, now) = (epoch() - DAY, epoch());
    let t = tree(&m, &a);
    fs::write(
        m.cfg.state.join("check-results"),
        format!("vm:{t} pass {old}\nvm:{t} fail\n"),
    )
    .unwrap();
    assert_eq!(memo.result_of("vm", &a), None);
    fs::write(
        m.cfg.state.join("check-results"),
        format!("vm:{t} pass {now}\nvm:{t} maybe {now}\n"),
    )
    .unwrap();
    assert_eq!(memo.result_of("vm", &a), None, "the last says neither");
    // A commit that isn't one has no result, and forced, nothing has.
    assert_eq!(memo.result_of("vm", "nope"), None);
    memo.record("vm", &a, Verdict::Pass).unwrap();
    let forced = Memo {
        cfg: &m.cfg,
        force: true,
    };
    assert_eq!(forced.result_of("vm", &a), None);
}

#[test]
fn passing_first_time_counts_up_and_failing_starts_again() {
    let m = Machine::new();
    assert_eq!(clean(&m.cfg, "vm", true), Ok(1));
    assert_eq!(clean(&m.cfg, "vm", true), Ok(2));
    assert_eq!(clean(&m.cfg, "other", true), Ok(1));
    assert_eq!(clean(&m.cfg, "vm", false), Ok(0));
    assert_eq!(clean(&m.cfg, "vm", true), Ok(1));
    assert_eq!(m.recorded("clean-vm").as_deref(), Some("1"));
    fs::write(m.cfg.state.join("clean-vm"), "garbled\n").unwrap();
    assert_eq!(clean(&m.cfg, "vm", true), Ok(1));
}
