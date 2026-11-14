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
    /// The failed jobs' steps, and the conclusion a finished run ends with
    /// (cancelled when unset).
    steps: Value,
    ended: Option<&'static str>,
}

impl Api for Hub {
    fn request(
        &mut self,
        method: &str,
        path: &str,
        _: Option<&Value>,
        _: Option<&str>,
    ) -> Result<Response, Failed> {
        self.asked.push((method.into(), path.into()));
        let (status, body) = if path.contains("/annotations") {
            if self.annotation_failure {
                return Err(Failed {
                    why: "connection lost".into(),
                    transient: true,
                });
            }
            (200, json!([]))
        } else if path.contains("/actions/jobs/") {
            (200, json!({"steps": self.steps}))
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
                "conclusion": if self.going { Value::Null } else { json!(self.ended.unwrap_or("cancelled")) }}),
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
        pr: json!({"baseRefName": "main", "commits": {"nodes": [{"commit": {
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
fn permission_refusal_recovers_next_pass_and_conflicts_are_not_permissions() {
    let a = ask(vec![check_run("build", 11, 7, "CANCELLED")]);
    let mut h = Hub {
        attempt: 1,
        status: 403,
        ..Hub::default()
    };
    assert!(run(&mut h, &a, false).unwrap().names[0].contains("Actions write"));
    h.status = 409;
    assert!(!run(&mut h, &a, false).unwrap().names[0].contains("Actions write"));
    h.status = 201;
    assert_eq!(run(&mut h, &a, false).unwrap().state, "pending");
    assert!(h.going);
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

fn steps(failed: &[&str]) -> Value {
    let mut all = vec![json!({"name": "Run bash \"$TOOLS/negative.sh\"", "conclusion": "success"})];
    all.extend(
        failed
            .iter()
            .map(|n| json!({"name": n, "conclusion": "failure"})),
    );
    all.push(json!({"name": "Complete job", "conclusion": "success"}));
    Value::Array(all)
}

const ATTEST_RUN: &str = "Run actions/attest@1e69f48acb82d1966a394da916b4c1698aa569d6";

#[test]
fn a_failed_attestation_upload_alone_is_run_again_once() {
    let mut h = Hub {
        attempt: 1,
        steps: steps(&[ATTEST_RUN]),
        ended: Some("failure"),
        ..Hub::default()
    };
    let a = ask(vec![check_run("train / negative", 11, 7, "FAILURE")]);
    let got = run(&mut h, &a, false).unwrap();
    assert_eq!(got.state, "pending");
    assert!(
        h.asked
            .iter()
            .any(|(m, p)| m == "POST" && p == "repos/sam/r/actions/runs/7/rerun-failed-jobs"),
        "{:?}",
        h.asked
    );
    assert_eq!(posts(&h), 1);
    // The rerun failed the same way: attempt 2 is red, with no second rerun.
    h.going = false;
    let got = run(&mut h, &a, false).unwrap();
    assert_eq!(got.state, "red");
    assert_eq!(
        got.names,
        ["train / negative (attestation upload failed again after a rerun)"]
    );
    assert_eq!(posts(&h), 1);
}

#[test]
fn any_other_failed_step_keeps_a_failure_red() {
    for failed in [
        &["Run bash \"$TOOLS/negative.sh\""][..],
        &["Run bash \"$TOOLS/negative.sh\"", ATTEST_RUN],
        &[],
    ] {
        let mut h = Hub {
            attempt: 1,
            steps: steps(failed),
            ended: Some("failure"),
            ..Hub::default()
        };
        let a = ask(vec![check_run("train / negative", 11, 7, "FAILURE")]);
        let got = run(&mut h, &a, false).unwrap();
        assert_eq!(got.state, "red", "{failed:?}");
        assert_eq!(got.names, ["train / negative"], "{failed:?}");
        assert_eq!(posts(&h), 0, "{failed:?}");
    }
    // A genuine failure elsewhere in the run reruns nothing.
    let mut h = Hub {
        attempt: 1,
        steps: steps(&[ATTEST_RUN]),
        ended: Some("failure"),
        ..Hub::default()
    };
    let mut lint = check_run("lint", 12, 7, "FAILURE");
    lint["databaseId"] = json!(0);
    let a = ask(vec![check_run("train / negative", 11, 7, "FAILURE"), lint]);
    assert_eq!(run(&mut h, &a, false).unwrap().state, "red");
    assert_eq!(posts(&h), 0);
}
