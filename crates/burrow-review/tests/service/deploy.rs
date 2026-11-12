//! A run outlives a switch while the admission policy parks its looks: it
//! holds them to the policy deployed now, not the one it started with, and
//! hands the queue to the new config's run, entries and sessions intact.
use crate::rig::{HEAD, Rig};
use serde_json::{Value, json};
use std::{
    fs, thread,
    time::{Duration, Instant},
};

fn until(what: &str, mut yes: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(20);
    while !yes() {
        assert!(Instant::now() < end, "{what}");
        thread::sleep(Duration::from_millis(20));
    }
}

fn entry(rig: &Rig, name: &str) -> Value {
    serde_json::from_slice(&fs::read(rig.path("queue").join(name)).unwrap()).unwrap()
}

#[test]
fn a_deploy_that_allows_a_parked_repo_hands_its_look_to_the_new_run() {
    let mut rig = Rig::new();
    let snap = rig.snapshot(5, HEAD);
    let parked = rig.queue(&snap, 5, HEAD, json!({"repo":"sam/new","opened":1,"at":2}));
    let queued = entry(&rig, &parked);
    let work = thread::spawn({
        let s = rig.deployed();
        move || burrow_review::work(s)
    });
    // Parked: sam/new isn't on the deployed allow list.
    thread::sleep(Duration::from_millis(300));
    assert!(!work.is_finished());
    assert!(rig.runs().is_empty());
    rig.cfg["settings"]["allowed"] = json!(["sam/r", "sam/new"]);
    rig.deploy();
    until("the parked run never handed over", || work.is_finished());
    work.join().unwrap();
    assert!(
        rig.runs().is_empty(),
        "the old run took a look after the switch"
    );
    assert_eq!(entry(&rig, &parked), queued, "the queue entry kept its age");
    assert!(rig.said().contains("a new config is deployed"));
    // The queue watcher starts the new config's run, which takes it.
    burrow_review::work(rig.deployed());
    assert_eq!(rig.runs().len(), 1);
    assert_eq!(rig.back(&parked)["result"], "review");
}

#[test]
fn a_deploy_that_drops_a_repo_vacates_its_look_and_keeps_its_session() {
    let mut rig = Rig::new();
    rig.agent(json!({"early":true, "sleep":{"#5":600}}));
    let snap = rig.snapshot(5, HEAD);
    let name = rig.queue(&snap, 5, HEAD, json!({"opened":1}));
    let work = thread::spawn({
        let s = rig.deployed();
        move || burrow_review::work(s)
    });
    until("the look never began", || rig.runs().len() == 1);
    let session = rig.runs()[0]["session"].clone();
    thread::sleep(Duration::from_millis(100));
    rig.cfg["settings"]["allowed"] = json!(["sam/q"]);
    rig.deploy();
    until("the look ran on under the old policy", || {
        work.is_finished()
    });
    work.join().unwrap();
    assert!(!rig.path("done").join(&name).exists());
    assert!(rig.path("queue").join(&name).exists());
    assert_eq!(rig.look(&snap)["id"], session);
    // Allowed again by the next deploy: the next run resumes its session.
    fs::write(rig.home().join(".fake/codex.json"), "{}").unwrap();
    rig.cfg["settings"]["allowed"] = json!(["sam/r"]);
    burrow_review::work(rig.deployed());
    let runs = rig.runs();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[1]["session"], session);
    assert_eq!(runs[1]["resumed"], true);
    assert_eq!(rig.back(&name)["result"], "review");
}
