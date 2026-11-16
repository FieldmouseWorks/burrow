//! Only protected queues select producer or judge execution and answers.
use crate::rig::{HEAD, Rig};
use serde_json::{Value, json};
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

fn rig() -> Rig {
    let mut rig = Rig::new();
    for dir in [
        "producer-queue",
        "judge-queue",
        "producer-done",
        "fresh",
        "producer-sessions",
    ] {
        fs::create_dir(rig.path(dir)).unwrap();
    }
    rig.cfg["producers"] = json!({"queue":rig.path("producer-queue"), "judgeQueue":rig.path("judge-queue"),
        "done":rig.path("producer-done"), "snapshots":rig.path("fresh"), "sessions":rig.path("producer-sessions")});
    rig
}

fn queue(rig: &Rig, judge: bool, extra: Value) -> (String, Vec<u8>) {
    let snap = "fresh-one";
    fs::create_dir_all(rig.path("fresh").join(snap)).unwrap();
    let name = rig.queue(snap, 5, HEAD, extra);
    let source = rig.path("queue").join(&name);
    let bytes = fs::read(&source).unwrap();
    fs::rename(
        source,
        rig.path(if judge {
            "judge-queue"
        } else {
            "producer-queue"
        })
        .join(&name),
    )
    .unwrap();
    (name, bytes)
}

fn back(rig: &Rig, name: &str) -> Value {
    let path = rig.path("producer-done").join(name);
    let mode = fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o640);
    assert_eq!(
        fs::metadata(&path).unwrap().uid(),
        rustix::process::geteuid().as_raw()
    );
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

#[test]
fn judges_run_fresh_with_their_schema_and_execution() {
    let rig = rig();
    rig.agent(json!({"answer":{"verdict":"pass","why":"It is reversible and checked."}}));
    let (name, bytes) = queue(
        &rig,
        true,
        json!({"fresh":{"model":"test-model","effort":"high"}}),
    );
    rig.work();
    let answer = back(&rig, &name);
    assert_eq!(answer["result"], "judgment");
    assert_eq!(answer["digest"], burrow_review::answer::digest(&bytes));
    assert_eq!(answer["ran"]["engine"], "codex");
    assert_eq!(answer["ran"]["model"], "test-model");
    assert_eq!(answer["ran"]["effort"], "high");
    let run = &rig.runs()[0];
    assert_eq!(answer["ran"]["session"], run["session"]);
    assert_eq!(run["schema"], burrow_review::judgment::schema());
    let args = run["args"].as_array().unwrap();
    assert!(args.contains(&json!("model=\"test-model\"")));
    assert!(args.contains(&json!("model_reasoning_effort=high")));
    assert!(args.contains(&json!("default_permissions=\"review\"")));
    assert_eq!(run["resumed"], false);
    assert!(rig.names("producer-sessions").is_empty());
    assert!(rig.names("sessions").is_empty());
    assert!(rig.names("done").is_empty());
    let first = answer["ran"]["session"].clone();
    queue(
        &rig,
        true,
        json!({"fresh":{"model":"test-model","effort":"high"}}),
    );
    rig.work();
    assert_ne!(back(&rig, &name)["ran"]["session"], first);
    assert_eq!(rig.runs()[1]["resumed"], false);
}

#[test]
fn a_fresh_queue_cannot_select_judgment_with_a_field() {
    let rig = rig();
    let (name, bytes) = queue(
        &rig,
        false,
        json!({"kind":"judge", "fresh":{"model":null,"effort":null}}),
    );
    rig.work();
    let answer = back(&rig, &name);
    assert_eq!(answer["result"], "review");
    assert_eq!(answer["digest"], burrow_review::answer::digest(&bytes));
    assert_eq!(rig.runs()[0]["schema"], burrow_review::codex::schema());
    fs::remove_dir_all(rig.path("fresh/fresh-one")).unwrap();
    rig.work();
    assert!(rig.names("producer-done").is_empty());
}

#[test]
fn a_steward_queue_cannot_select_any_protected_answer() {
    let rig = rig();
    for extra in [
        json!({"kind":"judge"}),
        json!({"fresh":{"model":"test-model","effort":"high"}}),
    ] {
        let name = rig.queue("fresh-forged", 5, HEAD, extra);
        rig.work();
        let answer = rig.back(&name);
        assert_eq!(answer["result"], "error");
        assert!(
            answer["error"]
                .as_str()
                .unwrap()
                .contains("protected queue")
        );
        assert!(rig.names("producer-done").is_empty());
    }
    assert!(rig.runs().is_empty());
}

#[test]
fn judges_require_execution_options_and_valid_verdicts_and_reasons() {
    for (fresh, said) in [
        (
            json!({"model":null,"effort":"high"}),
            json!({"verdict":"pass","why":"checked"}),
        ),
        (
            json!({"model":"test-model","effort":"high"}),
            json!({"verdict":"invented","why":"checked"}),
        ),
        (
            json!({"model":"test-model","effort":"high"}),
            json!({"verdict":"pass","why":" "}),
        ),
    ] {
        let rig = rig();
        rig.agent(json!({"answer":said}));
        let (name, _) = queue(&rig, true, json!({"fresh":fresh}));
        rig.work();
        assert_eq!(back(&rig, &name)["result"], "error");
    }
}

// Run the real worker loop while a different repo is either parked or
// actively reviewing. Each lifecycle case runs for both protected queues.
#[test]
fn released_answers_are_reclaimed_throughout_the_worker_lifecycle() {
    use std::{
        thread,
        time::{Duration, Instant},
    };
    fn until(mut ready: impl FnMut() -> bool) {
        let end = Instant::now() + Duration::from_secs(20);
        while !ready() {
            assert!(Instant::now() < end, "answer lifecycle did not advance");
            thread::sleep(Duration::from_millis(20));
        }
    }
    for judge in [false, true] {
        for parked in [false, true] {
            let rig = rig();
            rig.agent(json!({"sleep":{"keeper":600}}));
            let snap = rig.snapshot(9, HEAD);
            let keeper = rig.queue(
                &snap,
                9,
                HEAD,
                json!({
                    "repo": if parked {"sam/parked"} else {"sam/r"}, "prompt":"keeper"
                }),
            );
            let (name, _) = queue(
                &rig,
                judge,
                json!({"fresh":{"model":"test-model","effort":"high"}}),
            );
            // A completed answer retained across a restart: must stay while
            // its snapshot is present, then go when its owner releases it.
            burrow_review::answer::save(
                &rig.path("producer-done").join(&name),
                &json!({"old":true}),
            )
            .unwrap();
            let service = rig.service();
            let policy = service.cfg.settings.as_ref().unwrap().clone();
            let worker = thread::spawn(move || burrow_review::work(service));
            let answer = rig.path("producer-done").join(&name);
            until(|| {
                !rig.path(if judge {
                    "judge-queue"
                } else {
                    "producer-queue"
                })
                .join(&name)
                .exists()
            });
            assert!(
                answer.exists(),
                "collected an answer with a retained snapshot"
            );
            fs::remove_dir_all(rig.path("fresh/fresh-one")).unwrap();
            until(|| !answer.exists());
            assert!(!worker.is_finished(), "did not exercise a retained worker");

            // Abandonment while queued or running, including an answer
            // published after its snapshot disappeared, must also be swept.
            let (again, _) = queue(
                &rig,
                judge,
                json!({"repo":"sam/parked", "fresh":{"model":"test-model","effort":"high"}}),
            );
            let again_answer = rig.path("producer-done").join(&again);
            fs::remove_dir_all(rig.path("fresh/fresh-one")).unwrap();
            fs::remove_file(
                rig.path(if judge {
                    "judge-queue"
                } else {
                    "producer-queue"
                })
                .join(&again),
            )
            .unwrap();
            burrow_review::answer::save(&again_answer, &json!({"late":true})).unwrap();
            until(|| !again_answer.exists());

            // Release the other request without waiting for its fake agent.
            // The live admission overlay vacates running reviews as well.
            fs::remove_file(rig.path("queue").join(keeper)).unwrap();
            policy
                .command(&["pause".into(), "sam/r".into()], "test")
                .unwrap();
            until(|| worker.is_finished());
            worker.join().unwrap();

            // Restart cleanup covers abandoned answers left by a stopped run.
            burrow_review::answer::save(&again_answer, &json!({"orphan":true})).unwrap();
            rig.work();
            assert!(!again_answer.exists());
        }
    }
}

#[test]
fn answers_published_after_snapshot_removal_are_collected_before_exit() {
    for judge in [false, true] {
        let rig = rig();
        let (name, _) = queue(
            &rig,
            judge,
            json!({"fresh":{"model":"test-model","effort":"high"}}),
        );
        fs::remove_dir_all(rig.path("fresh/fresh-one")).unwrap();
        // The queued entry survives abandonment (or a crash in cleanup).
        // queued() publishes its error after seeing the missing snapshot.
        rig.work();
        assert!(rig.names("producer-done").is_empty());
        assert!(
            !rig.path(if judge {
                "judge-queue"
            } else {
                "producer-queue"
            })
            .join(name)
            .exists()
        );
    }
}
