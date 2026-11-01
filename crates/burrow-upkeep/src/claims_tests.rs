//! Which of a PR's fix jobs are stopped, kept or answered for.

use serde_json::{Value, json};

use super::*;
use crate::pass::Out;
use crate::testing::{Hub, States, T, answer as reply, config, look};

const HANDED: &str = "031dc0b1cea629bf7c7654091751c3fa6616e2ed";
const PUSHED: &str = "2a4e3e43eb0e039064eee1416e0d8a0a6a91c901";
const THEIRS: &str = "de738d4af1623f058ab3a2fbd3318fbd3d9e7de0";
const ACTIVITY: &str = "repos/sam/r/activity?ref=refs%2Fheads%2Ffeat&per_page=30$";

fn handing() -> Handing {
    Handing {
        kind: "handed".into(),
        head: HANDED.into(),
        body: "Does it.".into(),
        at: 1_793_558_866.0,
        looked: BTreeMap::new(),
    }
}

/// The PR at `head`, its job job-1 handed HANDED, its jobs pushing as
/// workers[bot] (or as the user, without `machinery`).
fn at(head: &str, jobs: &std::path::Path, machinery: bool) -> Look {
    Look {
        number: 5,
        head: head.into(),
        branch: "feat".into(),
        machinery: machinery.then(|| "workers[bot]".into()),
        jobs: jobs.into(),
        answering: BTreeMap::from([("job-1".into(), handing())]),
        pushed_by: None,
    }
}

/// One look at the PR, job-1 `state` (proj can't say, if none): what it
/// gave back, and the pass's own.
fn seen_as(hub: &mut Hub, state: Option<&str>, l: &Look) -> (Seen, Out) {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let mut jobs = States::new("running");
    jobs.default = state.map(String::from);
    let (out, got) = look(&cfg, hub, &mut jobs, 0.0, |p| Ok(claims(p, l)));
    (got.unwrap(), out)
}

fn seen(hub: &mut Hub, state: &str, l: &Look) -> (Seen, Out) {
    seen_as(hub, Some(state), l)
}

fn pushes(who: &str, head: &str) -> Value {
    json!([{"activity_type": "push", "before": HANDED, "after": head, "actor": {"login": who}}])
}

#[test]
fn a_job_on_a_head_someone_else_pushed_is_stopped() {
    let jobs = tempfile::tempdir().unwrap();
    let mut hub = Hub::default();
    hub.on("GET", ACTIVITY, reply(200, pushes("sam", THEIRS)));
    let (s, _) = seen(&mut hub, "running", &at(THEIRS, jobs.path(), true));
    assert_eq!(
        s.stop,
        vec![Stop {
            jid: "job-1".into(),
            why: "de738d4, the PR's head now, is sam's push, not its own".into()
        }]
    );
    assert!(s.answering.is_empty() && s.answered.is_empty());
    assert_eq!(s.pushed_by, Some((THEIRS.into(), "sam".into())));
    // Who pushed it is kept for the head: not asked again.
    let mut l = at(THEIRS, jobs.path(), true);
    l.pushed_by = s.pushed_by;
    let mut hub = Hub::default();
    let (s, _) = seen(&mut hub, "running", &l);
    assert_eq!(s.stop.len(), 1);
    assert!(hub.asked.is_empty(), "{:?}", hub.paths());
}

#[test]
fn a_job_whose_own_push_the_head_is_carries_on() {
    let jobs = tempfile::tempdir().unwrap();
    let mut hub = Hub::default();
    hub.on("GET", ACTIVITY, reply(200, pushes("workers[bot]", PUSHED)));
    let (s, _) = seen(&mut hub, "running", &at(PUSHED, jobs.path(), true));
    assert!(s.stop.is_empty());
    assert_eq!(s.answering["job-1"], handing());
    // Nor is one on the head it was handed asked about at all.
    let mut hub = Hub::default();
    let (s, _) = seen(&mut hub, "running", &at(HANDED, jobs.path(), true));
    assert!(s.stop.is_empty() && hub.asked.is_empty());
}

#[test]
fn a_push_github_cant_place_yet_is_looked_for_again() {
    let jobs = tempfile::tempdir().unwrap();
    let mut hub = Hub::default();
    hub.on("GET", ACTIVITY, reply(200, json!([])));
    let (s, out) = seen(&mut hub, "running", &at(THEIRS, jobs.path(), true));
    assert!(s.stop.is_empty() && s.pushed_by.is_none());
    assert_eq!(
        s.answering["job-1"].looked,
        BTreeMap::from([(THEIRS.into(), 1)])
    );
    assert_eq!(out.wake, Some(T + LOOK_AGAIN));
    // The last of its LOOKS books none.
    let mut l = at(THEIRS, jobs.path(), true);
    l.answering.get_mut("job-1").unwrap().looked = BTreeMap::from([(THEIRS.into(), LOOKS - 1)]);
    let mut hub = Hub::default();
    hub.on("GET", ACTIVITY, reply(200, json!([])));
    let (s, out) = seen(&mut hub, "running", &l);
    assert_eq!(s.answering["job-1"].looked[THEIRS], LOOKS);
    assert_eq!(out.wake, None);
}

#[test]
fn a_job_still_waiting_for_its_slot_is_stopped_without_asking() {
    let jobs = tempfile::tempdir().unwrap();
    for machinery in [true, false] {
        let mut hub = Hub::default();
        let (s, _) = seen(&mut hub, "queued", &at(THEIRS, jobs.path(), machinery));
        assert_eq!(
            s.stop,
            vec![Stop {
                jid: "job-1".into(),
                why: "it was queued for a head the PR no longer has (it's at de738d4 now)".into()
            }]
        );
        assert!(hub.asked.is_empty());
    }
}

#[test]
fn without_the_machinery_app_a_job_at_work_carries_on() {
    // Nor one proj can't say about.
    let jobs = tempfile::tempdir().unwrap();
    for state in [Some("running"), None] {
        let mut hub = Hub::default();
        let (s, _) = seen_as(&mut hub, state, &at(THEIRS, jobs.path(), false));
        assert!(s.stop.is_empty() && s.answering.contains_key("job-1"));
        assert!(hub.asked.is_empty());
    }
}

#[test]
fn a_job_thats_over_is_answered_for_once() {
    let jobs = tempfile::tempdir().unwrap();
    // It left nothing: forgotten, nothing to say.
    let mut hub = Hub::default();
    let (s, _) = seen(&mut hub, "done", &at(HANDED, jobs.path(), true));
    assert_eq!(s, Seen::default());
    // It left answers on a push someone else's head is past: forgotten,
    // and why.
    std::fs::create_dir(jobs.path().join("job-1")).unwrap();
    let left = json!({"pushed": PUSHED, "threads": []}).to_string();
    std::fs::write(jobs.path().join("job-1/answers.json"), &left).unwrap();
    let pr = |head: &str| {
        json!({"data": {"repository": {"pullRequest": {
            "state": "OPEN", "headRefOid": head, "headRefName": "feat", "body": "Does it.",
            "reviewThreads": {"pageInfo": {"hasNextPage": false}, "nodes": []},
        }}}})
    };
    let mut hub = Hub {
        graphql: vec![("reviewThreads(first: 100)".into(), reply(200, pr(THEIRS)))],
        ..Hub::default()
    };
    let (s, _) = seen(&mut hub, "failed", &at(THEIRS, jobs.path(), true));
    assert!(s.answering.is_empty() && s.stop.is_empty());
    assert_eq!(
        s.answered,
        vec![Answered {
            jid: "job-1".into(),
            kind: "handed".into(),
            outcome: "moved",
            why: Some("feat moved on to de738d4af162, which isn't the job's push".into())
        }]
    );
    // GitHub didn't answer: kept, to go again.
    let mut hub = Hub {
        graphql: vec![("reviewThreads(first: 100)".into(), reply(502, json!({})))],
        ..Hub::default()
    };
    let (s, _) = seen(&mut hub, "done", &at(PUSHED, jobs.path(), true));
    assert_eq!(s.answering["job-1"], handing());
    assert_eq!(s.answered[0].outcome, "unanswered");
    // An answers file that can't be read is no answer.
    std::fs::remove_file(jobs.path().join("job-1/answers.json")).unwrap();
    std::fs::create_dir(jobs.path().join("job-1/answers.json")).unwrap();
    let mut hub = Hub::default();
    let (s, _) = seen(&mut hub, "done", &at(PUSHED, jobs.path(), true));
    assert!(s.answering.is_empty() && hub.asked.is_empty());
    assert_eq!(s.answered[0].outcome, "wrong");
    assert!(
        s.answered[0]
            .why
            .as_deref()
            .unwrap()
            .starts_with("its answers file can't be read")
    );
}
