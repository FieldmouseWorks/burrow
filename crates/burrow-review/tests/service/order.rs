//! Who goes first: a reviewer's next free place goes to the lead repos
//! first, then to the rest of the pipeline's own, then to the oldest PR's;
//! one look at a time in a session; and what passes queue meanwhile is
//! taken in the same run.

use std::fs;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::json;

use crate::rig::{HEAD, Rig};

/// Lead repos first (`leadRepos`, any case), then the rest of the
/// pipeline's own (`pipelineRepos`), then the oldest PR's: by when it was
/// opened, so one pushed again (a-old, its new head queued after c-new's)
/// keeps its place.
#[test]
fn the_pipelines_own_go_first_then_the_oldest() {
    let mut rig = Rig::new();
    rig.cfg["reviewers"]["codex"]["parallel"] = json!(1);
    for (n, repo, opened, at) in [
        (1, "sam/r", 1, 5),
        (2, "sam/pipe", 3, 3),
        (3, "sam/r", 2, 2),
        (4, "sam/lead", 4, 4),
    ] {
        let snap = rig.snapshot(n, HEAD);
        rig.queue(
            &snap,
            n,
            HEAD,
            json!({"repo": repo, "opened": opened, "at": at,
                                          "prompt": format!("Review #{n} in {repo}.")}),
        );
    }
    rig.work();
    let order: Vec<String> = rig
        .runs()
        .iter()
        .map(|r| r["prompt"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        order,
        [
            "Review #4 in sam/lead.",
            "Review #2 in sam/pipe.",
            "Review #1 in sam/r.",
            "Review #3 in sam/r."
        ]
    );
    let mut done: Vec<(f64, String)> = rig
        .names("done")
        .iter()
        .map(|n| (rig.back(n)["at"].as_f64().unwrap(), n.clone()))
        .collect();
    done.sort_by(|a, b| a.0.total_cmp(&b.0));
    let numbers: Vec<&str> = done
        .iter()
        .map(|(_, n)| n.split('-').nth(4).unwrap())
        .collect();
    assert_eq!(numbers, ["4", "2", "1", "3"]);
}

/// Two reviews at once, as `parallel` says.
#[test]
fn parallel_looks_run_side_by_side() {
    let rig = Rig::new();
    rig.agent(json!({"sleep": 2}));
    for n in [1, 2] {
        let snap = rig.snapshot(n, HEAD);
        rig.queue(&snap, n, HEAD, json!({}));
    }
    let t = Instant::now();
    rig.work();
    let runs = rig.runs();
    assert_eq!(runs.len(), 2);
    assert!(
        (runs[0]["began"].as_f64().unwrap() - runs[1]["began"].as_f64().unwrap()).abs() < 1.5,
        "{runs:?}"
    );
    assert!(t.elapsed() < Duration::from_secs(4), "{:?}", t.elapsed());
}

/// One look at a time in a session: a PR's second head waits for the look
/// at its first, then resumes its session.
#[test]
fn a_prs_heads_take_turns_in_its_session() {
    let rig = Rig::new();
    rig.agent(json!({"sleep": 2}));
    for head in ["a".repeat(40), "b".repeat(40)] {
        let snap = rig.snapshot(5, &head);
        rig.queue(&snap, 5, &head, json!({}));
    }
    rig.work();
    let runs = rig.runs();
    let [one, two] = &runs[..] else {
        panic!("{runs:?}")
    };
    assert!(
        two["began"].as_f64().unwrap() - one["began"].as_f64().unwrap() >= 2.0,
        "{runs:?}"
    );
    assert_eq!(two["resumed"], true);
    assert_eq!(two["session"], one["session"]);
}

/// A look done with and queued again (a retry) while another (PR 7's) is
/// still running is taken again, not left until the service next starts.
#[test]
fn one_queued_again_meanwhile_is_taken_again() {
    let rig = Rig::new();
    rig.agent(json!({"sleep": {"#7 in sam/r": 3}}));
    let five = rig.snapshot(5, HEAD);
    let seven = rig.snapshot(7, HEAD);
    let again = rig.queue(&five, 5, HEAD, json!({}));
    rig.queue(&seven, 7, HEAD, json!({}));
    let entry = fs::read(rig.path("queue").join(&again)).unwrap();
    let s = rig.service();
    let worker = thread::spawn(move || burrow_review::work(s));
    while !rig.path("done").join(&again).exists() {
        thread::sleep(Duration::from_millis(50));
    }
    rig.back(&again);
    fs::write(rig.path("queue").join(&again), entry).unwrap();
    worker.join().unwrap();
    assert!(rig.names("queue").is_empty());
    assert_eq!(rig.back(&again)["result"], "review");
    let mut sevens: Vec<bool> = rig
        .runs()
        .iter()
        .map(|r| r["prompt"].as_str().unwrap().contains("#7 in sam/r"))
        .collect();
    sevens.sort();
    assert_eq!(sevens, [false, false, true]);
}
