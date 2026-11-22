//! The VM tests take a heavy slot on a machine that has them, and say how
//! the test went either way, with the stand-ins for runuser, nix and heavy
//! (testing.rs).

use std::fs;

use super::*;
use crate::testing::{Machine, bin, sha};

#[test]
fn a_vm_test_waits_for_a_heavy_slot_where_the_machine_has_them() {
    let mut m = Machine::new();
    let a = sha('a');
    let main = m.main();
    let log = m.path("check-agents.log");
    let want = format!(
        "build --no-link -L git+file://{}?rev={a}#checks.x86_64-linux.agents\n",
        m.cfg.work.display()
    );
    m.cfg.heavy = Some(bin().join("heavy"));
    assert!(run_check(
        &m.cfg,
        &Limit::begin(&m.cfg),
        "agents",
        &a,
        &main,
        &log
    ));
    assert_eq!(m.kept("nix.log"), format!("slot taken\n{want}"));
    fs::write(m.home("check-fails"), "").unwrap();
    assert!(!run_check(
        &m.cfg,
        &Limit::begin(&m.cfg),
        "agents",
        &a,
        &main,
        &log
    ));

    // Without slots it just runs.
    m.cfg.heavy = None;
    fs::write(m.home("nix.log"), "").unwrap();
    assert!(!run_check(
        &m.cfg,
        &Limit::begin(&m.cfg),
        "agents",
        &a,
        &main,
        &log
    ));
    fs::remove_file(m.home("check-fails")).unwrap();
    assert!(run_check(
        &m.cfg,
        &Limit::begin(&m.cfg),
        "agents",
        &a,
        &main,
        &log
    ));
    assert_eq!(m.kept("nix.log"), format!("{want}{want}"));
}

#[test]
fn a_vm_tests_log_is_all_it_said_and_only_this_runs() {
    let m = Machine::new();
    let a = sha('a');
    let main = m.main();
    let log = m.path("check-agents.log");
    fs::write(&log, "the run before\n").unwrap();
    assert!(run_check(
        &m.cfg,
        &Limit::begin(&m.cfg),
        "agents",
        &a,
        &main,
        &log
    ));
    let said = fs::read_to_string(&log).unwrap();
    assert!(!said.contains("the run before"), "{said}");
    assert!(said.contains("ran build --no-link -L"), "{said}");
    assert!(said.contains("a VM test's log"), "{said}");
    // A log it can't write: no test run, and no pass.
    assert!(!run_check(
        &m.cfg,
        &Limit::begin(&m.cfg),
        "agents",
        &a,
        &main,
        &m.path("none/check.log")
    ));
    assert_eq!(m.kept("nix.log").lines().count(), 1);
}

#[test]
fn a_slot_that_finds_main_moved_runs_no_vm_test() {
    let mut m = Machine::new();
    m.cfg.heavy = Some(bin().join("heavy"));
    let main = m.main();
    let log = m.path("check-agents.log");
    // main moves while it waits for its slot.
    fs::write(
        m.home("in-slot"),
        format!(
            "git -C {} push -q origin HEAD:main",
            m.path("other").display()
        ),
    )
    .unwrap();
    m.git(
        m.dir.path(),
        &[
            "clone",
            "-q",
            m.path("origin.git").to_str().unwrap(),
            "other",
        ],
    );
    m.git(
        &m.path("other"),
        &["commit", "-q", "--allow-empty", "-m", "landed"],
    );
    assert!(!run_check(
        &m.cfg,
        &Limit::begin(&m.cfg),
        "agents",
        &main,
        &main,
        &log
    ));
    assert_eq!(m.kept("nix.log"), "slot taken\n", "no VM test ran");
    assert!(fs::read_to_string(&log).unwrap().contains("main moved to"));
    // Its look is what the run reads after: the clone knows main moved.
    assert_ne!(m.work(&["rev-parse", "refs/remotes/origin/main"]), main);
}
