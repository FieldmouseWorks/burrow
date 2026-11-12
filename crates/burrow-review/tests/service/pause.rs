//! Runtime policy vacates a review without losing its queue entry or session.
use crate::rig::{HEAD, Rig};
use serde_json::json;
use std::{
    thread,
    time::{Duration, Instant},
};

fn until(mut yes: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(15);
    while !yes() {
        assert!(Instant::now() < end, "review did not reach its checkpoint");
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_pause_frees_the_reviewer_and_resumes_its_native_session() {
    let mut rig = Rig::new();
    rig.cfg["reviewers"]["codex"]["parallel"] = json!(1);
    rig.agent(json!({"early":true, "sleep":{"#5":600}}));
    let snap = rig.snapshot(5, HEAD);
    let paused = rig.queue(&snap, 5, HEAD, json!({"opened":1}));
    let other_snap = rig.snapshot(6, HEAD);
    let other = rig.queue(&other_snap, 6, HEAD, json!({"repo":"sam/pipe", "opened":2}));
    // Select #5 first; the next allowed review can use only its vacated slot.
    rig.cfg["leadRepos"] = json!([]);
    rig.cfg["pipelineRepos"] = json!([]);
    let service = rig.service();
    let policy = service.cfg.settings.as_ref().unwrap().clone();
    let work = thread::spawn(move || burrow_review::work(service));
    until(|| rig.runs().len() == 1);
    let session = rig.runs()[0]["session"].clone();
    thread::sleep(Duration::from_millis(100));
    policy
        .command(&["pause".into(), "sam/r".into()], "test")
        .unwrap();
    until(|| rig.path("done").join(&other).exists());
    assert!(!rig.path("done").join(&paused).exists());
    assert!(rig.path("queue").join(&paused).exists());
    assert!(rig.path("trees").join(&snap).is_dir());
    std::fs::write(rig.home().join(".fake/codex.json"), "{}").unwrap();
    policy
        .command(&["allow".into(), "sam/r".into()], "test")
        .unwrap();
    work.join().unwrap();
    let runs = rig.runs();
    assert_eq!(runs.len(), 3);
    assert_eq!(runs[2]["session"], session);
    assert_eq!(runs[2]["resumed"], true);
    assert_eq!(rig.back(&paused)["result"], "review");
}
