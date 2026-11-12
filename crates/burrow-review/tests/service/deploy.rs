//! A run outlives a switch while the admission policy parks its looks: it
//! holds them to the policy deployed now, not the one it started with, and
//! hands the queue to the deployed generation's program, entries and
//! sessions intact. Its own config and program stay one generation's.
use crate::rig::{HEAD, Rig};
use burrow_review::Ended;
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
fn a_deploy_that_allows_a_parked_repo_hands_its_look_to_the_new_program() {
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
    assert_eq!(work.join().unwrap(), Ended::Moved);
    assert!(
        rig.runs().is_empty(),
        "the old run took a look after the switch"
    );
    assert_eq!(entry(&rig, &parked), queued, "the queue entry kept its age");
    assert!(rig.said().contains("another generation is deployed"));
    // The deployed program, with its own config, takes it.
    assert_eq!(burrow_review::work(rig.deployed()), Ended::Done);
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
    assert_eq!(work.join().unwrap(), Ended::Moved);
    assert!(!rig.path("done").join(&name).exists());
    assert!(rig.path("queue").join(&name).exists());
    assert_eq!(rig.look(&snap)["id"], session);
    // Allowed again by the next deploy: its program resumes the session.
    fs::write(rig.home().join(".fake/codex.json"), "{}").unwrap();
    rig.cfg["settings"]["allowed"] = json!(["sam/r"]);
    assert_eq!(burrow_review::work(rig.deployed()), Ended::Done);
    let runs = rig.runs();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[1]["session"], session);
    assert_eq!(runs[1]["resumed"], true);
    assert_eq!(rig.back(&name)["result"], "review");
}

/// systemd starts the unit it has: between a switch putting the new /etc in
/// place and reloading the units, that's the old generation's program and
/// config. It takes nothing, and hands the queue to the deployed program.
#[test]
fn a_run_from_a_unit_not_yet_reloaded_takes_nothing() {
    let mut rig = Rig::new();
    let snap = rig.snapshot(5, HEAD);
    let name = rig.queue(&snap, 5, HEAD, json!({}));
    let old = rig.cfg.clone();
    rig.cfg["timeoutMinutes"] = json!(31);
    assert_eq!(burrow_review::work(rig.started(&old)), Ended::Moved);
    assert!(rig.runs().is_empty());
    assert!(rig.path("queue").join(&name).exists());
    // Nor does one whose generation was rolled back past this one's.
    fs::remove_dir_all(rig.path("deployed")).unwrap();
    let s = rig.started(&rig.cfg);
    fs::remove_dir_all(rig.path("deployed")).unwrap();
    assert_eq!(burrow_review::work(s), Ended::Moved);
    assert!(rig.runs().is_empty());
    assert_eq!(burrow_review::work(rig.deployed()), Ended::Done);
    assert_eq!(rig.back(&name)["result"], "review");
}

/// The hand-over itself, as systemd runs the service: the deployed program
/// takes the run's place, with its own config, not this run's.
#[test]
fn the_service_hands_over_to_the_deployed_program() {
    let mut rig = Rig::new();
    let old = rig.path("own.json");
    fs::write(&old, rig.cfg.to_string()).unwrap();
    rig.cfg["timeoutMinutes"] = json!(31);
    let dir = rig.deploy();
    let said = rig.path("next.txt");
    burrow_test::executable(
        dir.join("burrow-review"),
        format!(
            "#!{}\necho \"${{REVIEW_CONFIG-none}} $*\" > {}\n",
            crate::rig::which("bash").display(),
            said.display()
        ),
    );
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_burrow-review"))
        .arg("work")
        .env("REVIEW_CONFIG", &old)
        .env("REVIEW_DEPLOYED", &dir)
        .env("HOME", rig.home())
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(fs::read_to_string(&said).unwrap(), "none work\n");
}

/// The generation that takes the queue runs its agents with its own tools,
/// the ones its sandbox's store holds, not the PATH the old unit started
/// the run with.
#[test]
fn the_new_generation_runs_its_agents_with_its_own_tools() {
    let mut rig = Rig::new();
    let snap = rig.snapshot(5, HEAD);
    let name = rig.queue(&snap, 5, HEAD, json!({}));
    let old = rig.cfg.clone();
    let tools = format!(
        "{}:/nix/store/ffff-new-tools/bin",
        old["path"].as_str().unwrap()
    );
    rig.cfg["path"] = json!(tools);
    assert_eq!(burrow_review::work(rig.started(&old)), Ended::Moved);
    assert_eq!(burrow_review::work(rig.deployed()), Ended::Done);
    let runs = rig.runs();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0]["env"]["PATH"], tools.as_str());
    assert_eq!(rig.back(&name)["result"], "review");
}
