//! CI recovery keeps one verdict owner, dry-run and typed errors.

use super::*;
use crate::pass::Out;
use crate::testing::{States, config};
use burrow_github::{Api, Failed, Response};
use serde_json::json;

#[derive(Default)]
struct Hub {
    asked: Vec<(String, String)>,
    attempt: u64,
    going: bool,
    status: u16,
    lost: bool,
    annotation_failure: bool,
    /// The runs' event: GitHub reruns no `dynamic` one, whoever asks.
    event: &'static str,
    /// What update-branch answers: 202, unless set.
    update: u16,
    /// The bodies of the update-branch asks.
    updates: Vec<Value>,
}

impl Api for Hub {
    fn request(
        &mut self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        _: Option<&str>,
    ) -> Result<Response, Failed> {
        self.asked.push((method.into(), path.into()));
        let (status, body) = if method == "PUT" {
            assert!(path.ends_with("/pulls/5/update-branch"), "{path}");
            self.updates.push(body.cloned().unwrap_or_default());
            match self.update {
                0 => (202, json!({"message": "Updating pull request branch."})),
                s => (
                    s,
                    json!({"message": "There are no new commits on the base branch."}),
                ),
            }
        } else if method == "POST" && self.event == "dynamic" {
            (
                403,
                json!({"message": "This workflow run cannot be retried"}),
            )
        } else if path.contains("/annotations") {
            if self.annotation_failure {
                return Err(Failed {
                    why: "connection lost".into(),
                    transient: true,
                });
            }
            (200, json!([]))
        } else if method == "POST" {
            if self.status == 0 || self.status == 201 || self.lost {
                self.attempt += 1;
                self.going = true;
            }
            if self.lost {
                return Err(Failed {
                    why: "lost response".into(),
                    transient: true,
                });
            }
            (
                if self.status == 0 { 201 } else { self.status },
                json!({"message": "refused"}),
            )
        } else {
            (
                200,
                json!({"run_attempt": self.attempt.max(1), "status": if self.going { "queued" } else { "completed" },
                "conclusion": if self.going { Value::Null } else { json!("cancelled") }, "event": self.event}),
            )
        };
        Ok(Response {
            status,
            body: body.to_string(),
            headers: vec![],
        })
    }
    fn pause(&mut self, _: u64) {}
}

fn check_run(name: &str, id: u64, run: u64, conclusion: &str) -> Value {
    json!({"__typename": "CheckRun", "name": name, "databaseId": id, "status": "COMPLETED", "conclusion": conclusion,
        "checkSuite": {"id": format!("s{run}"), "status": "COMPLETED", "app": {"slug": "github-actions"},
            "workflowRun": {"databaseId": run, "workflow": {"id": format!("w{run}"), "name": "CI"}}}})
}

fn ask(checks: Vec<Value>) -> Ask {
    Ask {
        pr: json!({"number": 5, "headRefOid": "a".repeat(40), "baseRefName": "main", "commits": {"nodes": [{"commit": {
        "statusCheckRollup": {"contexts": {"nodes": checks}}}}]}}),
        proofs: vec![],
        moved: None,
        remote: true,
        recover: true,
    }
}

fn run(h: &mut Hub, a: &Ask, dry: bool) -> Result<Verdict, Error> {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let mut jobs = States::new("running");
    let mut p = Pass {
        cfg: &cfg,
        repo: "sam/r",
        now: 2000.0,
        dry,
        searched: 0,
        api: h,
        jobs: &mut jobs,
        out: Out::default(),
    };
    check(&mut p, a)
}

fn posts(h: &Hub) -> usize {
    h.asked.iter().filter(|(m, _)| m == "POST").count()
}

fn puts(h: &Hub) -> usize {
    h.asked.iter().filter(|(m, _)| m == "PUT").count()
}

#[test]
fn recovery_is_once_per_run_across_observers_and_names() {
    let mut h = Hub {
        attempt: 1,
        ..Hub::default()
    };
    let a = ask(vec![
        check_run("build", 11, 7, "CANCELLED"),
        check_run("build", 12, 7, "CANCELLED"),
    ]);
    assert_eq!(run(&mut h, &a, false).unwrap().state, "pending");
    assert_eq!(posts(&h), 1);
    assert_eq!(run(&mut h, &a, false).unwrap().state, "pending");
    assert_eq!(posts(&h), 1);
    h.going = false;
    let got = run(&mut h, &a, false).unwrap();
    assert_eq!(got.state, "red");
    assert_eq!(got.names, ["build (cancelled again after a rerun)"]);
    assert_eq!(posts(&h), 1);
}

#[test]
fn dry_and_classification_only_never_write() {
    let mut h = Hub {
        attempt: 1,
        ..Hub::default()
    };
    let mut a = ask(vec![check_run("build", 11, 7, "CANCELLED")]);
    assert_eq!(run(&mut h, &a, true).unwrap().state, "pending");
    assert_eq!(posts(&h), 0);
    a.recover = false;
    assert_eq!(run(&mut h, &a, false).unwrap().state, "pending");
    assert_eq!(posts(&h), 0);
}

#[test]
fn unanswered_write_keeps_its_type_and_is_not_submitted_twice() {
    let mut h = Hub {
        attempt: 1,
        lost: true,
        ..Hub::default()
    };
    let a = ask(vec![check_run("build", 11, 7, "CANCELLED")]);
    assert!(run(&mut h, &a, false).unwrap_err().unanswered);
    assert_eq!(posts(&h), 1);
    h.lost = false;
    h.going = false;
    assert_eq!(run(&mut h, &a, false).unwrap().state, "red");
    assert_eq!(posts(&h), 1);
}

#[test]
fn annotation_failure_keeps_its_retry_type() {
    let mut h = Hub {
        annotation_failure: true,
        ..Hub::default()
    };
    let a = ask(vec![check_run("build", 11, 7, "CANCELLED")]);
    assert!(run(&mut h, &a, false).unwrap_err().unanswered);
    assert_eq!(posts(&h), 0);
}

#[test]
fn a_refused_rerun_quotes_github_and_is_asked_again_next_pass() {
    let a = ask(vec![check_run("build", 11, 7, "CANCELLED")]);
    let mut h = Hub {
        attempt: 1,
        status: 403,
        ..Hub::default()
    };
    // GitHub's words, never a guess at why (a permission it may not lack).
    assert_eq!(
        run(&mut h, &a, false).unwrap().names,
        ["build (cancelled; not run again: HTTP 403: refused)"]
    );
    h.status = 409;
    assert_eq!(
        run(&mut h, &a, false).unwrap().names,
        ["build (cancelled; not run again: HTTP 409: refused)"]
    );
    h.status = 201;
    assert_eq!(run(&mut h, &a, false).unwrap().state, "pending");
    assert!(h.going);
    assert_eq!(posts(&h), 3);
}

#[test]
fn a_cancelled_dynamic_run_brings_the_branch_up_to_date_instead_of_a_rerun() {
    // CodeQL's default setup: GitHub answers any rerun of it with a 403
    // (the Hub's, for a dynamic run), so none is asked for at all.
    let mut h = Hub {
        attempt: 1,
        event: "dynamic",
        ..Hub::default()
    };
    let a = ask(vec![
        check_run("Analyze (rust)", 11, 7, "CANCELLED"),
        check_run("Analyze (python)", 12, 7, "CANCELLED"),
    ]);
    let got = run(&mut h, &a, false).unwrap();
    assert_eq!(got.state, "pending");
    assert_eq!(
        got.names,
        [
            "Analyze (rust) (cancelled; GitHub reruns no dynamic run: its branch is being updated)",
            "Analyze (python) (cancelled; GitHub reruns no dynamic run: its branch is being updated)",
        ]
    );
    assert_eq!(posts(&h), 0);
    // One ask for the head the verdict is of, never one pushed since.
    assert_eq!(h.updates, [json!({"expected_head_sha": "a".repeat(40)})]);

    // Not behind its base: GitHub's no is quoted, and asked again next pass.
    h.update = 422;
    let got = run(&mut h, &a, false).unwrap();
    assert_eq!(got.state, "pending");
    assert_eq!(
        got.names[0],
        "Analyze (rust) (cancelled; GitHub reruns no dynamic run; updating its branch said \
         HTTP 422: There are no new commits on the base branch.)"
    );
    assert_eq!((posts(&h), puts(&h)), (0, 2));

    // Dry, or only classifying: nothing written.
    run(&mut h, &a, true).unwrap();
    let mut only = ask(
        a.pr["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["contexts"]["nodes"]
            .as_array()
            .unwrap()
            .clone(),
    );
    only.recover = false;
    run(&mut h, &only, false).unwrap();
    assert_eq!((posts(&h), puts(&h)), (0, 2));
}

#[test]
fn a_dynamic_run_beside_a_genuine_failure_is_left_to_the_failure() {
    let mut h = Hub {
        attempt: 1,
        event: "dynamic",
        ..Hub::default()
    };
    let a = ask(vec![
        check_run("Analyze (rust)", 11, 7, "CANCELLED"),
        check_run("Analyze (python)", 12, 7, "FAILURE"),
    ]);
    assert_eq!(run(&mut h, &a, false).unwrap().state, "red");
    assert_eq!((posts(&h), puts(&h)), (0, 0));
}

#[test]
fn a_never_started_dynamic_run_brings_the_branch_up_to_date() {
    let mut h = Hub {
        attempt: 1,
        event: "dynamic",
        ..Hub::default()
    };
    let got = run(&mut h, &unstarted(), false).unwrap();
    assert_eq!(
        got.names,
        ["CI (never started; GitHub reruns no dynamic run: its branch is being updated)"]
    );
    assert_eq!((posts(&h), puts(&h)), (0, 1));
}

#[test]
fn genuine_failures_and_unfinished_suites_never_rerun() {
    let mut h = Hub::default();
    let a = ask(vec![
        check_run("build", 11, 7, "CANCELLED"),
        check_run("lint", 12, 7, "FAILURE"),
    ]);
    assert_eq!(run(&mut h, &a, false).unwrap().state, "red");
    assert_eq!(posts(&h), 0);
    let mut c = check_run("build", 11, 7, "CANCELLED");
    c["checkSuite"]["status"] = json!("IN_PROGRESS");
    assert_eq!(run(&mut h, &ask(vec![c]), false).unwrap().state, "pending");
    assert_eq!(posts(&h), 0);
}

#[test]
fn separate_runs_keep_their_own_allowance() {
    let mut h = Hub {
        attempt: 1,
        ..Hub::default()
    };
    let a = ask(vec![
        check_run("build", 11, 7, "CANCELLED"),
        check_run("build", 12, 8, "CANCELLED"),
    ]);
    run(&mut h, &a, false).unwrap();
    assert_eq!(posts(&h), 2);
}

fn unstarted() -> Ask {
    let mut a = ask(vec![]);
    a.pr["commits"]["nodes"][0]["commit"]["checkSuites"] = json!({"nodes": [{
        "id": "s7", "status": "QUEUED", "checkRuns": {"totalCount": 0},
        "workflowRun": {"databaseId": 7, "createdAt": "1970-01-01T00:00:00Z",
            "workflow": {"id": "w7", "name": "CI"}}
    }]});
    a
}

#[test]
fn unstarted_runs_recover_whole_workflow_once_across_observers() {
    let mut h = Hub {
        attempt: 1,
        ..Hub::default()
    };
    let a = unstarted();
    let got = run(&mut h, &a, false).unwrap();
    assert_eq!(got.state, "pending");
    assert_eq!(got.names, ["CI (never started, to be run again)"]);
    assert_eq!(posts(&h), 1);
    assert!(
        h.asked
            .iter()
            .any(|(m, p)| m == "POST" && p.ends_with("/7/rerun"))
    );
    run(&mut h, &a, false).unwrap();
    assert_eq!(posts(&h), 1);
}

#[test]
fn unstarted_runs_preserve_dry_mode_and_uncertain_submission() {
    let mut h = Hub {
        attempt: 1,
        ..Hub::default()
    };
    let a = unstarted();
    run(&mut h, &a, true).unwrap();
    assert_eq!(posts(&h), 0);
    h.lost = true;
    assert!(run(&mut h, &a, false).unwrap_err().unanswered);
    assert_eq!(posts(&h), 1);
    h.lost = false;
    h.going = false;
    run(&mut h, &a, false).unwrap();
    assert_eq!(posts(&h), 1);
}

#[test]
fn only_old_complete_snapshots_without_jobs_authorize_unstarted_recovery() {
    for (field, value) in [
        ("status", json!("IN_PROGRESS")),
        ("checkRuns", json!({"totalCount": 1})),
        (
            "workflowRun",
            json!({"databaseId": 7, "createdAt": "1970-01-01T00:30:00Z"}),
        ),
    ] {
        let mut h = Hub::default();
        let mut a = unstarted();
        a.pr["commits"]["nodes"][0]["commit"]["checkSuites"]["nodes"][0][field] = value;
        run(&mut h, &a, false).unwrap();
        assert_eq!(posts(&h), 0, "{field}");
    }
    let mut h = Hub::default();
    let mut a = unstarted();
    a.pr["commits"]["nodes"][0]["commit"]["checkSuites"]["pageInfo"] = json!({"hasNextPage": true});
    assert_eq!(run(&mut h, &a, false).unwrap().state, "pending");
    assert_eq!(posts(&h), 0);
}

fn house_run(id: u64, suite: &str, conclusion: &str) -> Value {
    json!({"__typename": "CheckRun", "databaseId": id, "name": "house", "status": "COMPLETED",
           "conclusion": conclusion,
           "checkSuite": {"id": suite, "status": "COMPLETED", "app": {"slug": "github-actions"},
                          "workflowRun": {"file": {"path": ".github/workflows/house.yml"},
                                          "workflow": {"id": "house", "name": "house"}}}})
}

#[test]
fn a_later_completed_run_replaces_earlier_work_but_a_skip_or_older_one_does_not() {
    let none = BTreeSet::new();
    let ids = |c: &[Value]| -> Vec<u64> {
        ci_runs::effective(c, &none, false)
            .iter()
            .map(|x| x["databaseId"].as_u64().unwrap())
            .collect()
    };
    // The newer pass replaces an older failure or cancellation, in any order.
    for old in ["FAILURE", "CANCELLED"] {
        assert_eq!(
            ids(&[house_run(10, "s1", old), house_run(11, "s2", "SUCCESS")]),
            [11]
        );
        assert_eq!(
            ids(&[house_run(11, "s2", "SUCCESS"), house_run(10, "s1", old)]),
            [11]
        );
        // A skip did no work; an older success cannot replace a newer check.
        assert_eq!(
            ids(&[house_run(10, "s1", old), house_run(11, "s2", "SKIPPED")]),
            [10, 11]
        );
        assert_eq!(
            ids(&[house_run(10, "s1", "SUCCESS"), house_run(11, "s2", old)]),
            [11]
        );
    }
    // A cancelled check is not cleared by a newer cancelled one.
    assert_eq!(
        ids(&[
            house_run(10, "s1", "CANCELLED"),
            house_run(11, "s2", "CANCELLED")
        ]),
        [10, 11]
    );
    // Another workflow's namesake clears nothing.
    let mut other = house_run(11, "s2", "SUCCESS");
    other["checkSuite"]["workflowRun"]["workflow"]["id"] = json!("other");
    other["checkSuite"]["workflowRun"]["file"]["path"] = json!(".github/workflows/other.yml");
    assert_eq!(ids(&[house_run(10, "s1", "FAILURE"), other]), [10, 11]);
}
