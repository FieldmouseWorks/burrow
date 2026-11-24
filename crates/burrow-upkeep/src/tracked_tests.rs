//! A tracked cause outside the PR: open, nothing is run again; closed, each
//! red run from before it closed is, once, and one since is the head's.

use serde_json::{Value, json};

use super::*;
use crate::testing::{Hub, States, answer, config, look};

fn check_run(name: &str, id: u64, run: u64, conclusion: &str) -> Value {
    json!({"__typename": "CheckRun", "name": name, "databaseId": id, "status": "COMPLETED", "conclusion": conclusion,
        "checkSuite": {"id": format!("s{run}"), "status": "COMPLETED", "app": {"slug": "github-actions"},
            "workflowRun": {"databaseId": run, "workflow": {"id": format!("w{run}"), "name": "CI"}}}})
}

fn asked(tracker: &str, checks: Vec<Value>) -> Ask {
    Ask {
        tracker: tracker.into(),
        ci: ci::Ask {
            pr: json!({"number": 5, "headRefOid": "a".repeat(40), "baseRefName": "main", "commits": {"nodes": [{"commit": {
            "statusCheckRollup": {"contexts": {"nodes": checks}}}}]}}),
            proofs: vec![],
            moved: None,
            remote: true,
            recover: false,
        },
    }
}

fn seen(hub: &mut Hub, a: &Ask, dry: bool) -> Result<Tracked, String> {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let (_, got) = look(&cfg, hub, &mut States::new("done"), 0.0, |p| {
        p.dry = dry;
        tracked(p, a).map_err(|e| e.why)
    });
    got
}

fn run(status: &str, started: &str) -> Value {
    json!({"run_attempt": 1, "status": status, "conclusion": "failure", "run_started_at": started})
}

fn posts(hub: &Hub) -> Vec<&str> {
    hub.asked
        .iter()
        .filter(|a| a.0 == "POST")
        .map(|a| a.1.as_str())
        .collect()
}

const CLOSED: &str = "2026-11-23T12:00:00Z";

#[test]
fn an_open_tracker_runs_nothing_again() {
    let mut hub = Hub::default();
    hub.on(
        "GET",
        "repos/sam/infra/issues/7$",
        answer(200, json!({"state": "open", "closed_at": null})),
    );
    let a = asked(
        "sam/infra#7",
        vec![check_run("train / negative", 11, 70, "FAILURE")],
    );
    let got = seen(&mut hub, &a, false).unwrap();
    assert_eq!(
        got,
        Tracked {
            open: true,
            ..Tracked::default()
        }
    );
    assert!(posts(&hub).is_empty(), "{:?}", hub.paths());
}

#[test]
fn a_closed_tracker_runs_each_red_run_from_before_it_again_whole() {
    let mut hub = Hub::default();
    hub.on(
        "GET",
        "repos/sam/infra/issues/7$",
        answer(200, json!({"state": "closed", "closed_at": CLOSED})),
    );
    // Run 70 failed before it closed, run 71 since (a rerun's own verdict),
    // run 72 is going (a rerun asked already), run 73 passed.
    hub.on(
        "GET",
        "repos/sam/r/actions/runs/70$",
        answer(200, run("completed", "2026-11-23T09:00:00Z")),
    );
    hub.on(
        "GET",
        "repos/sam/r/actions/runs/71$",
        answer(200, run("completed", "2026-11-23T12:30:00Z")),
    );
    hub.on(
        "GET",
        "repos/sam/r/actions/runs/72$",
        answer(200, run("queued", "2026-11-23T09:00:00Z")),
    );
    hub.on(
        "POST",
        "repos/sam/r/actions/runs/70/rerun$",
        answer(201, json!({})),
    );
    let a = asked(
        "sam/infra#7",
        vec![
            check_run("train / negative", 11, 70, "FAILURE"),
            check_run("train / negative", 21, 70, "FAILURE"),
            check_run("lint", 12, 71, "FAILURE"),
            check_run("build", 13, 72, "TIMED_OUT"),
            check_run("test", 14, 73, "SUCCESS"),
        ],
    );
    let got = seen(&mut hub, &a, false).unwrap();
    assert_eq!(
        got,
        Tracked {
            reran: vec!["70".into()],
            going: vec!["72".into()],
            since: vec!["71".into()],
            ..Tracked::default()
        }
    );
    assert_eq!(posts(&hub), ["repos/sam/r/actions/runs/70/rerun"]);
    // GitHub's no is said, in its words, and nothing more is asked.
    hub.on(
        "POST",
        "repos/sam/r/actions/runs/70/rerun$",
        answer(403, json!({"message": "too old"})),
    );
    let got = seen(&mut hub, &a, false).unwrap();
    assert!(
        got.reran.is_empty() && got.refused.contains_key("70"),
        "{got:?}"
    );
    // A dry pass asks for nothing.
    let got = seen(&mut hub, &a, true).unwrap();
    assert_eq!(got.reran, ["70"]);
    assert!(posts(&hub).is_empty(), "{:?}", hub.paths());
}

#[test]
fn a_tracker_github_has_none_of_is_nothing_to_wait_on() {
    let mut hub = Hub::default();
    hub.on(
        "GET",
        "repos/sam/infra/issues/7$",
        answer(404, json!({"message": "Not Found"})),
    );
    let a = asked("sam/infra#7", vec![check_run("lint", 11, 70, "FAILURE")]);
    let got = seen(&mut hub, &a, false).unwrap();
    assert!(
        !got.open && got.unknown.is_some() && posts(&hub).is_empty(),
        "{got:?}"
    );
    let got = seen(&mut Hub::default(), &asked("infra#7", vec![]), false).unwrap();
    assert!(!got.open && got.unknown.is_some(), "{got:?}");
}
