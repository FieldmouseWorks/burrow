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
        taken: None,
        posted: None,
        waits: false,
        resumes: false,
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
    std::fs::create_dir_all(l.jobs.join("job-1")).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let mut jobs = States::new("running");
    jobs.default = state.map(String::from);
    jobs.record = Some(l.jobs.clone());
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
    std::fs::create_dir_all(jobs.path().join("job-1")).unwrap();
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

#[test]
fn comment_engine_comes_from_the_finished_turn() {
    for (engine, other, label, unsafe_field) in [
        ("claude", "codex", "Claude", None),
        ("codex", "claude", "Codex", None),
        ("codex", "claude", "Claude", Some("turns")),
        ("codex", "claude", "Claude", Some("2.engine")),
        ("codex", "claude", "Claude", Some("traversal")),
    ] {
        let jobs = tempfile::tempdir().unwrap();
        let dir = jobs.path().join("job-1");
        std::fs::create_dir(&dir).unwrap();
        for (name, text) in [
            ("turns", "2"),
            ("engine", other),
            ("1.engine", "claude"),
            ("2.engine", engine),
        ] {
            std::fs::write(dir.join(name), text).unwrap();
        }
        if let Some(field) = unsafe_field {
            if field == "traversal" {
                std::fs::write(jobs.path().join("outside.engine"), "codex").unwrap();
                std::fs::write(dir.join("turns"), "../outside").unwrap();
            } else {
                let outside = jobs.path().join("outside");
                std::fs::write(&outside, if field == "turns" { "2" } else { "codex" }).unwrap();
                std::fs::remove_file(dir.join(field)).unwrap();
                std::os::unix::fs::symlink(&outside, dir.join(field)).unwrap();
            }
        }
        std::fs::write(
            dir.join("answers.json"),
            json!({"comment":"Done."}).to_string(),
        )
        .unwrap();
        let mut hub = Hub {
            graphql: vec![(
                "reviewThreads(first: 100)".into(),
                reply(
                    200,
                    json!({"data":{"repository":{"pullRequest":{
                        "state":"OPEN", "headRefOid":HANDED, "body":"Does it.",
                        "reviewThreads":{"nodes":[],"pageInfo":{"hasNextPage":false}}
                    }}}}),
                ),
            )],
            ..Hub::default()
        };
        hub.on(
            "GET",
            "repos/sam/r/issues/5/comments?",
            reply(200, json!([])),
        );
        hub.on(
            "POST",
            "repos/sam/r/issues/5/comments$",
            reply(201, json!({"id":1})),
        );
        let (s, _) = seen(&mut hub, "done", &at(HANDED, jobs.path(), true));
        assert_eq!(s.answered[0].outcome, "posted");
        assert!(hub.asked.iter().any(|(method, path, _, body)| {
            method == "POST"
                && path == "repos/sam/r/issues/5/comments"
                && body
                    .as_ref()
                    .is_some_and(|v| v["body"] == format!("**{label} job**\n\nDone."))
        }));
    }
}

#[test]
fn answer_files_cannot_follow_a_job_symlink_to_host_text() {
    let jobs = tempfile::tempdir().unwrap();
    let job = jobs.path().join("job-1");
    std::fs::create_dir(&job).unwrap();
    let private = jobs.path().join("outside");
    std::fs::write(
        &private,
        json!({"comment":"ordinary confidential text"}).to_string(),
    )
    .unwrap();
    std::os::unix::fs::symlink(&private, job.join("answers.json")).unwrap();
    let mut hub = Hub::default();
    let (s, _) = seen(&mut hub, "done", &at(HANDED, jobs.path(), true));
    assert_eq!(s.answered[0].outcome, "wrong");
    assert!(hub.asked.is_empty());
}

#[test]
fn external_awaits_keep_every_fix_claim_until_its_final_answers() {
    for kind in ["handed", "redFixed", "failFixed", "conflictFixed"] {
        for own_push in [false, true] {
            for intermediate_answers in [false, true] {
                let jobs = tempfile::tempdir().unwrap();
                let dir = jobs.path().join("job-1");
                std::fs::create_dir(&dir).unwrap();
                let head = if own_push { PUSHED } else { HANDED };
                let mut l = at(HANDED, jobs.path(), true);
                l.answering.get_mut("job-1").unwrap().kind = kind.into();
                if intermediate_answers {
                    std::fs::write(dir.join("answers.json"), "intermediate answers").unwrap();
                }
                for (turn, upstream) in ["sam/up#3", "sam/other#9", "sam/up#3"]
                    .into_iter()
                    .enumerate()
                {
                    std::fs::write(dir.join("turns"), (turn + 1).to_string()).unwrap();
                    std::fs::write(
                        dir.join("await"),
                        format!("SAM/R#5\t0\t{{}}\n{upstream}\t0\t{{}}\n"),
                    )
                    .unwrap();
                    l.head = head.into();
                    let mut hub = Hub::default();
                    hub.on("GET", ACTIVITY, reply(200, pushes("workers[bot]", head)));
                    let (s, _) = seen(&mut hub, "done", &l);
                    assert_eq!(s.answering.len(), 1, "{kind}");
                    assert!(s.stop.is_empty() && s.answered.is_empty());
                    l.answering = s.answering;
                    l.pushed_by = s.pushed_by;
                    std::fs::write(dir.join("turns"), (turn + 2).to_string()).unwrap();
                    for cached in [false, true] {
                        if !cached {
                            l.pushed_by = None;
                        }
                        let mut hub = Hub::default();
                        hub.on("GET", ACTIVITY, reply(200, pushes("workers[bot]", head)));
                        for state in ["queued", "running"] {
                            let (s, _) = seen(&mut hub, state, &l);
                            assert_eq!(s.answering, l.answering, "{kind} {state} cached={cached}");
                            assert!(s.stop.is_empty());
                            l.pushed_by = s.pushed_by;
                        }
                    }
                }
                // Final answers replace any intermediate file, and go up
                // once its last turn holds nothing.
                std::fs::remove_file(dir.join("await")).unwrap();
                std::fs::write(dir.join("answers.json"), json!({
                    "pushed": own_push.then_some(head), "threads": [], "comment": "Finished the fix."
                }).to_string()).unwrap();
                let mut hub = Hub {
                    graphql: vec![(
                        "reviewThreads(first: 100)".into(),
                        reply(
                            200,
                            json!({
                                "data": {"repository": {"pullRequest": {
                                    "state": "OPEN", "headRefOid": head, "headRefName": "feat", "body": "Does it.",
                                    "reviewThreads": {"pageInfo": {"hasNextPage": false}, "nodes": []}
                                }}}
                            }),
                        ),
                    )],
                    ..Hub::default()
                };
                hub.on(
                    "GET",
                    "repos/sam/r/issues/5/comments?",
                    reply(200, json!([])),
                );
                hub.on(
                    "POST",
                    "repos/sam/r/issues/5/comments$",
                    reply(201, json!({"id": 1})),
                );
                let (s, _) = seen(&mut hub, "done", &l);
                assert!(s.answering.is_empty());
                assert_eq!(s.answered[0].outcome, "posted");
                assert!(hub.paths().iter().any(|p| p.contains("issues/5/comments")));
            }
        }
    }
}

/// GitHub with PR 5 at `head`, its description `body` and `threads`,
/// taking replies, resolves, a description and a comment.
fn github(head: &str, body: &str, threads: Value) -> Hub {
    let mut hub = Hub {
        graphql: vec![
            (
                "reviewThreads(first: 100)".into(),
                reply(
                    200,
                    json!({"data": {"repository": {"pullRequest": {
                        "state": "OPEN", "headRefOid": head, "headRefName": "feat", "body": body,
                        "reviewThreads": {"pageInfo": {"hasNextPage": false}, "nodes": threads},
                    }}}}),
                ),
            ),
            (
                "addPullRequestReviewThreadReply".into(),
                reply(
                    200,
                    json!({"data": {"addPullRequestReviewThreadReply": {"comment": {"id": "c"}}}}),
                ),
            ),
            (
                "resolveReviewThread".into(),
                reply(
                    200,
                    json!({"data": {"resolveReviewThread": {"thread": {"isResolved": true}}}}),
                ),
            ),
        ],
        ..Hub::default()
    };
    hub.on(
        "GET",
        "repos/sam/r/pulls/5$",
        reply(
            200,
            json!({"state": "open", "head": {"sha": head}, "body": body}),
        ),
    );
    hub.on("PATCH", "repos/sam/r/pulls/5$", reply(200, json!({})));
    hub.on(
        "GET",
        "repos/sam/r/issues/5/comments?",
        reply(200, json!([])),
    );
    hub.on(
        "POST",
        "repos/sam/r/issues/5/comments$",
        reply(201, json!({"id": 1})),
    );
    hub.on("GET", ACTIVITY, reply(200, pushes("workers[bot]", PUSHED)));
    hub
}

/// What the pass wrote to GitHub: each mutation's name, or a REST call.
fn writes(hub: &Hub) -> Vec<String> {
    hub.asked
        .iter()
        .filter_map(|(method, path, _, body)| {
            let q = body
                .as_ref()
                .and_then(|b| b["query"].as_str())
                .unwrap_or_default();
            if path == "graphql" {
                q.starts_with("mutation").then(|| {
                    if q.contains("addPullRequestReviewThreadReply") {
                        "reply"
                    } else {
                        "resolve"
                    }
                    .into()
                })
            } else {
                (method != "GET").then(|| format!("{method} {path}"))
            }
        })
        .collect()
}

// PSIV#459: a fix job ends each turn awaiting its own PR, which proj
// resumes it on. Its first turn's answers (a blocker, no push) went up and
// its claim went with them, so its second turn's push and answers were
// never read. Each turn's new answers go up now, on the head its own push
// left, and none twice.
#[test]
fn each_turn_awaiting_its_own_pr_has_its_new_answers_posted_once() {
    let jobs = tempfile::tempdir().unwrap();
    let dir = jobs.path().join("job-1");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("await"), "SAM/R#5\t0\t{}\n").unwrap();
    let mut l = at(HANDED, jobs.path(), true);
    let pass = |l: &mut Look, hub: &mut Hub, state: &str| {
        let (s, _) = seen(hub, state, l);
        l.answering = s.answering.clone();
        l.pushed_by = s.pushed_by.clone();
        s
    };
    // Turn 1: blocked, nothing pushed, a comment.
    std::fs::write(dir.join("turns"), "1").unwrap();
    std::fs::write(
        dir.join("answers.json"),
        json!({"pushed": null, "threads": [], "description": null, "comment": "Blocked."})
            .to_string(),
    )
    .unwrap();
    let mut hub = github(HANDED, "Does it.", json!([]));
    let s = pass(&mut l, &mut hub, "done");
    assert_eq!(s.answered[0].outcome, "posted");
    assert_eq!(writes(&hub), ["POST repos/sam/r/issues/5/comments"]);
    // What went up resumes it: still at work, for the steward.
    let kept = &s.answering["job-1"];
    assert!(kept.taken.is_some() && kept.head == HANDED);
    assert!(kept.waits && kept.resumes && kept.posted == Some(1));
    // The same answers, looked at again: nothing asked, nothing posted.
    let mut hub = Hub::default();
    let s = pass(&mut l, &mut hub, "done");
    assert!(s.answered.is_empty() && hub.asked.is_empty());
    assert_eq!(s.answering.len(), 1);
    // Turn 2, at work: it pushes, then writes its new answers.
    std::fs::write(dir.join("turns"), "2").unwrap();
    l.head = PUSHED.into();
    let mut hub = github(PUSHED, "Does it.", json!([]));
    let s = pass(&mut l, &mut hub, "running");
    assert!(s.answered.is_empty() && s.stop.is_empty());
    assert!(!s.answering["job-1"].waits && !s.answering["job-1"].resumes);
    std::fs::write(
        dir.join("answers.json"),
        json!({"pushed": PUSHED, "threads": [{"id": "T1", "fixed": true, "reply": "Fixed."}],
               "description": "Does it, fixed.", "comment": "Done."})
        .to_string(),
    )
    .unwrap();
    // Its turn over, awaiting the PR again: they go up, and its claim now
    // stands on its own push and the description it wrote.
    let open = json!([{"id": "T1", "isResolved": false, "comments": {"nodes": [{"body": "**Codex · P1** a race"}]}}]);
    let mut hub = github(PUSHED, "Does it.", open);
    let s = pass(&mut l, &mut hub, "done");
    assert_eq!(s.answered[0].outcome, "posted", "{:?}", s.answered);
    assert_eq!(
        writes(&hub),
        [
            "reply",
            "resolve",
            "PATCH repos/sam/r/pulls/5",
            "POST repos/sam/r/issues/5/comments"
        ]
    );
    let kept = &s.answering["job-1"];
    assert_eq!(
        (kept.head.as_str(), kept.body.as_str()),
        (PUSHED, "Does it, fixed.")
    );
    assert!(kept.waits && kept.resumes && kept.posted == Some(2));
    // Turn 3 leaves its answers as they were: none replayed, and nothing
    // of its resumes it, so for the steward it has ended (a new fix job
    // stops it first).
    std::fs::write(dir.join("turns"), "3").unwrap();
    let mut hub = Hub::default();
    let s = pass(&mut l, &mut hub, "done");
    assert!(s.answered.is_empty() && hub.asked.is_empty());
    let kept = &s.answering["job-1"];
    assert!(kept.waits && !kept.resumes && kept.posted == Some(2));
    // Turn 4 says one more thing on the PR as its description now is, and
    // ends holding nothing: that goes up, and its claim is over.
    std::fs::write(dir.join("turns"), "4").unwrap();
    std::fs::remove_file(dir.join("await")).unwrap();
    std::fs::write(
        dir.join("answers.json"),
        json!({"pushed": PUSHED, "threads": [], "description": "Does it, fixed.", "comment": "CI is green."})
            .to_string(),
    )
    .unwrap();
    let mut hub = github(PUSHED, "Does it, fixed.", json!([]));
    let s = pass(&mut l, &mut hub, "done");
    assert_eq!(s.answered[0].outcome, "posted", "{:?}", s.answered);
    assert_eq!(writes(&hub), ["POST repos/sam/r/issues/5/comments"]);
    assert!(s.answering.is_empty());
}

// Between turns, a job awaiting its own PR is still at work on the branch:
// someone else's push stops it, as it would a running one, and nothing of
// its goes up.
#[test]
fn a_job_awaiting_its_own_pr_stops_for_another_authors_push() {
    let jobs = tempfile::tempdir().unwrap();
    let dir = jobs.path().join("job-1");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("await"), "sam/r#5\t0\t{}\n").unwrap();
    std::fs::write(dir.join("turns"), "2").unwrap();
    std::fs::write(
        dir.join("answers.json"),
        json!({"pushed": null, "comment": "Hm."}).to_string(),
    )
    .unwrap();
    let mut hub = Hub::default();
    hub.on("GET", ACTIVITY, reply(200, pushes("sam", THEIRS)));
    let (s, _) = seen(&mut hub, "done", &at(THEIRS, jobs.path(), true));
    assert_eq!(s.stop.len(), 1, "{s:?}");
    assert!(s.answering.is_empty() && s.answered.is_empty());
    assert!(writes(&hub).is_empty());
}

#[test]
fn externally_awaiting_jobs_still_stop_for_another_authors_push() {
    let jobs = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(jobs.path().join("job-1")).unwrap();
    std::fs::write(jobs.path().join("job-1/await"), "sam/up#3\t0\t{}\n").unwrap();
    for kind in ["handed", "redFixed", "failFixed", "conflictFixed"] {
        for state in ["done", "queued", "running"] {
            for cached in [false, true] {
                let mut l = at(THEIRS, jobs.path(), true);
                l.answering.get_mut("job-1").unwrap().kind = kind.into();
                if cached {
                    l.pushed_by = Some((THEIRS.into(), "sam".into()));
                }
                let mut hub = Hub::default();
                hub.on("GET", ACTIVITY, reply(200, pushes("sam", THEIRS)));
                std::fs::write(jobs.path().join("job-1/turns"), "2").unwrap();
                let (s, _) = seen(&mut hub, state, &l);
                assert_eq!(s.stop.len(), 1);
                assert!(s.answering.is_empty() && s.answered.is_empty());
            }
        }
    }
}

#[test]
fn a_resume_cannot_clear_awaits_between_state_and_claim_completion() {
    use crate::pass::{Jobs, Lifecycle};
    use std::sync::mpsc;
    struct Resuming {
        dir: PathBuf,
        resume: Option<std::thread::JoinHandle<()>>,
    }
    impl Jobs for Resuming {
        fn lifecycle(&mut self, _: &str) -> Lifecycle {
            // State has just read done. A concurrent start tries to publish
            // turn 2 and exec clears awaits, before claims reads them.
            let dir = self.dir.clone();
            let (tx, rx) = mpsc::channel();
            self.resume = Some(std::thread::spawn(move || {
                let lock = std::fs::OpenOptions::new()
                    .append(true)
                    .open(dir.join("lock"))
                    .unwrap();
                assert!(
                    lock.try_lock().is_err(),
                    "claims must exclude concurrent starts"
                );
                tx.send(()).unwrap();
                lock.lock().unwrap();
                std::fs::write(dir.join("turns"), "2").unwrap();
                std::fs::remove_file(dir.join("await")).unwrap();
            }));
            rx.recv().unwrap();
            let text = std::fs::read_to_string(self.dir.join("await")).unwrap_or_default();
            Lifecycle {
                state: Some("done".into()),
                holds: text
                    .lines()
                    .filter_map(|l| l.split('\t').next())
                    .map(|t| format!("await {t}"))
                    .collect(),
            }
        }
        fn find(&mut self, _: &str) -> Result<Option<String>, String> {
            unreachable!()
        }
    }
    for kind in ["handed", "redFixed", "failFixed", "conflictFixed"] {
        let jobs = tempfile::tempdir().unwrap();
        let dir = jobs.path().join("job-1");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("turns"), "1").unwrap();
        std::fs::write(dir.join("lock"), "").unwrap();
        std::fs::write(dir.join("await"), "sam/up#3\t0\t{}\n").unwrap();
        std::fs::write(dir.join("answers.json"), "intermediate answers").unwrap();
        let mut l = at(HANDED, jobs.path(), true);
        l.answering.get_mut("job-1").unwrap().kind = kind.into();
        let cfg_dir = tempfile::tempdir().unwrap();
        let cfg = config(cfg_dir.path());
        let mut jobs = Resuming { dir, resume: None };
        let mut hub = Hub::default();
        let (_, got) = look(&cfg, &mut hub, &mut jobs, 0.0, |p| Ok(claims(p, &l)));
        let s = got.unwrap();
        jobs.resume.take().unwrap().join().unwrap();
        assert_eq!(s.answering, l.answering);
        assert!(s.answered.is_empty() && s.stop.is_empty() && hub.asked.is_empty());
    }
}

// A fix job whose turn ended with a question for a person (or a one-way
// one left for the owner) holds its claim as one awaiting upstream does:
// its answers wait for the turn its answer resumes, never posted as its
// last (remi#881). Once it holds nothing, they go up.
#[test]
fn a_question_keeps_the_claim_until_its_final_answers() {
    for held in ["ask", "owner"] {
        let jobs = tempfile::tempdir().unwrap();
        let dir = jobs.path().join("job-1");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("answers.json"), "intermediate answers").unwrap();
        let l = at(HANDED, jobs.path(), true);
        let cfg_dir = tempfile::tempdir().unwrap();
        let cfg = config(cfg_dir.path());
        let mut states = States::new("done");
        states.held.insert("job-1".into(), vec![held.into()]);
        let mut hub = Hub::default();
        let (_, got) = look(&cfg, &mut hub, &mut states, 0.0, |p| Ok(claims(p, &l)));
        let s = got.unwrap();
        assert_eq!(s.answering, l.answering, "{held}");
        assert!(
            s.answered.is_empty() && s.stop.is_empty() && hub.asked.is_empty(),
            "{held}"
        );
        states.held.insert("job-1".into(), vec![]);
        let (_, got) = look(&cfg, &mut hub, &mut states, 0.0, |p| Ok(claims(p, &l)));
        let s = got.unwrap();
        assert!(
            s.answering.is_empty() && s.answered[0].outcome == "wrong",
            "{held}: {:?}",
            s.answered
        );
    }
}
