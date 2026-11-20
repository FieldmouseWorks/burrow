//! What a run's look at main finds, on a machine of stand-ins (testing.rs).

use std::fs;

use burrow_github::fake::Fake;

use super::*;
use crate::testing::{Machine, bin, lock, sha};

#[test]
fn main_moved_is_where_origin_has_it_now() {
    let m = Machine::new();
    let main = m.main();
    assert_eq!(moved(&m.cfg.work, &main), None);
    m.landed("README", "notes");
    assert_eq!(moved(&m.cfg.work, &main), Some(m.main()));
    // The clone heard it too, for the run to read after.
    assert_eq!(m.work(&["rev-parse", "refs/remotes/origin/main"]), m.main());
}

#[test]
fn an_origin_it_cant_reach_is_no_sign_main_moved() {
    let m = Machine::new();
    let main = m.main();
    m.landed("README", "notes");
    fs::rename(m.path("origin.git"), m.path("gone.git")).unwrap();
    assert_eq!(moved(&m.cfg.work, &main), None);
}

/// main's next landing, ready in a clone of its own but not pushed: the
/// commit, and what pushes it (for a stand-in's hook).
fn next_landing(m: &Machine, nixpkgs: char) -> (String, String) {
    let other = m.path("next");
    m.git(
        m.dir.path(),
        &[
            "clone",
            "-q",
            m.path("origin.git").to_str().unwrap(),
            "next",
        ],
    );
    fs::write(
        other.join("flake.lock"),
        lock(&[("nixpkgs", &sha(nixpkgs)), ("burrow", &sha('b'))]) + "\n",
    )
    .unwrap();
    m.git(&other, &["commit", "-qam", "landed"]);
    let commit = m.git(&other, &["rev-parse", "HEAD"]);
    (
        commit,
        format!("git -C {} push -q origin HEAD:main\n", other.display()),
    )
}

/// The machine running main, with heavy slots, and main moved past it by
/// a landing (nixpkgs at c): a run's to put live.
fn landed() -> (Machine, String) {
    let mut m = Machine::running_main(&[("nixpkgs", &sha('a')), ("burrow", &sha('b'))]);
    m.cfg.heavy = Some(bin().join("heavy"));
    m.landed(
        "flake.lock",
        &lock(&[("nixpkgs", &sha('c')), ("burrow", &sha('b'))]),
    );
    let main = m.main();
    (m, main)
}

/// The commits it ran a VM test on, in order.
fn tested(m: &Machine) -> Vec<String> {
    m.kept("nix.log")
        .lines()
        .filter(|l| l.contains("#checks."))
        .filter_map(|l| Some(l.split("?rev=").nth(1)?.split('#').next()?.to_string()))
        .collect()
}

fn go(m: &Machine) -> u8 {
    crate::run::once(&m.cfg, &mut Fake::default(), false)
}

fn said(m: &Machine) -> String {
    m.status()["message"].as_str().unwrap().into()
}

fn locked(m: &Machine) -> String {
    fs::read_to_string(m.running() + "/flake.lock").unwrap()
}

#[test]
fn main_moving_while_it_waits_for_a_slot_is_tested_and_put_live_instead() {
    let (m, first) = landed();
    let (head, push) = next_landing(&m, 'd');
    // The landing after it comes while the run waits for its first slot.
    fs::write(m.home("in-slot"), push).unwrap();
    assert_eq!(go(&m), 0);
    assert_ne!(head, first);
    assert_eq!(tested(&m), [head.as_str()], "never a VM test of {first}");
    assert_eq!(said(&m), format!("live and confirmed: main {}", &head[..7]));
    assert!(locked(&m).contains(&sha('d')));
    assert_eq!(m.did("calls").matches("switch --toplevel").count(), 1);
}

#[test]
fn main_moving_after_its_vm_tests_is_never_switched_to() {
    let (m, first) = landed();
    let (head, push) = next_landing(&m, 'd');
    // It lands as the run's VM test of the first ends, which passes.
    fs::write(m.home("after-check"), push).unwrap();
    assert_eq!(go(&m), 0);
    assert_eq!(tested(&m), [first, head.clone()]);
    assert_eq!(said(&m), format!("live and confirmed: main {}", &head[..7]));
    assert!(locked(&m).contains(&sha('d')));
    assert_eq!(m.did("calls").matches("switch --toplevel").count(), 1);
}

#[test]
fn a_vm_test_main_moved_under_counts_for_nothing() {
    let (m, first) = landed();
    let (head, push) = next_landing(&m, 'd');
    // The first tree fails its test, but main moved while it ran.
    fs::write(m.home("after-check"), push).unwrap();
    fs::write(
        m.home("check-breaks"),
        format!(
            "\"nixpkgs\": {{\"locked\": {{\"rev\": \"{}\"}}}}\n",
            sha('c')
        ),
    )
    .unwrap();
    assert_eq!(go(&m), 0);
    // Not run again as flaky, nor on main as the updates' fault: no job.
    assert_eq!(tested(&m), [first.clone(), head.clone()]);
    assert_eq!(m.kept("proj.log"), "");
    assert_eq!(said(&m), format!("live and confirmed: main {}", &head[..7]));
    assert_eq!(
        crate::memo::Memo {
            cfg: &m.cfg,
            force: false
        }
        .result_of("vm", &first),
        None,
        "no result kept for a tree it left"
    );
}
