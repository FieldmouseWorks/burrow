use super::*;
use burrow_github::{Failed, Response};
use serde_json::json;
use std::collections::VecDeque;
fn producer() -> Producer<'static> {
    Producer {
        repo: "o/r",
        workflow: "flake",
        branch: "main",
        tip: "abc",
        key: "kept-abc",
    }
}
fn run(branch: &str, status: &str, conclusion: &str) -> Value {
    json!({"name":"flake", "event":"push", "head_sha":"abc", "head_branch":branch, "status":status, "conclusion":conclusion})
}
fn caches(saved: bool) -> Value {
    json!({"actions_caches": if saved { vec![json!({"key":"kept-abc","ref":"refs/heads/main"})] } else { vec![] }})
}
#[test]
fn only_the_exact_default_branch_push_can_justify_waiting() {
    let p = producer();
    for field in ["name", "event", "head_sha", "head_branch"] {
        let mut other = run("main", "in_progress", "");
        other[field] = json!("other");
        assert!(!p.running(&json!({"workflow_runs":[other]})), "{field}");
    }
    assert!(p.running(
        &json!({"workflow_runs":[run("other", "in_progress", ""), run("main", "in_progress", "")]})
    ));
    assert!(!p.running(&json!({"workflow_runs":[run("other", "in_progress", ""), run("main", "completed", "failure")]})));
    assert!(p.runs_path().contains("branch=main"));
}
#[test]
fn cache_queries_preserve_values_and_only_accept_an_exact_key_and_ref() {
    let p = Producer {
        workflow: "CI +&%#",
        branch: "main +&%#",
        tip: "abc+&%#",
        key: "kept-CI +&%#",
        ..producer()
    };
    assert_eq!(
        p.caches_path(),
        "repos/o/r/actions/caches?key=kept-CI%20%2B%26%25%23&ref=refs%2Fheads%2Fmain%20%2B%26%25%23&per_page=100"
    );
    assert_eq!(
        p.runs_path(),
        "repos/o/r/actions/runs?event=push&head_sha=abc%2B%26%25%23&branch=main%20%2B%26%25%23&per_page=100"
    );
    assert!(p.cached(
        &json!({"actions_caches":[{"key":p.key,"ref":format!("refs/heads/{}", p.branch)}]})
    ));
    let p = producer();
    for (key, reference) in [
        ("kept-abc-extra", "refs/heads/main"),
        ("kept-abc", "refs/heads/other"),
    ] {
        assert!(!p.cached(&json!({"actions_caches":[{"key":key,"ref":reference}]})));
    }
}
struct Script {
    replies: VecDeque<(String, Value)>,
}
impl Api for Script {
    fn request(
        &mut self,
        method: &str,
        path: &str,
        _: Option<&Value>,
        _: Option<&str>,
    ) -> std::result::Result<Response, Failed> {
        assert_eq!(method, "GET");
        let (expected, body) = self.replies.pop_front().expect("unexpected request");
        assert_eq!(path, expected);
        Ok(Response {
            status: 200,
            headers: vec![],
            body: body.to_string(),
        })
    }
}
#[test]
fn completion_during_planning_or_polling_keeps_even_a_failed_runs_saved_cache() {
    let p = producer();
    for already_finished in [true, false] {
        for conclusion in ["success", "failure"] {
            for saved in [true, false] {
                let mut replies = VecDeque::new();
                if !already_finished {
                    replies.extend([
                        (
                            p.runs_path(),
                            json!({"workflow_runs":[run("main","in_progress","")]}),
                        ),
                        (p.caches_path(), caches(false)),
                    ]);
                }
                replies.extend([
                    (
                        p.runs_path(),
                        json!({"workflow_runs":[run("main","completed",conclusion)]}),
                    ),
                    (p.caches_path(), caches(saved)),
                ]);
                let mut api = Script { replies };
                assert_eq!(
                    poll(Duration::from_secs(5), Duration::ZERO, || p.state(&mut api)).unwrap(),
                    if saved { Outcome::Saved } else { Outcome::Gone }
                );
                assert!(api.replies.is_empty());
            }
        }
    }
}
#[test]
fn saves_between_status_and_cache_are_seen_and_an_unrelated_push_does_not_wait() {
    let p = producer();
    for (branch, saved, expected) in [
        ("main", true, Outcome::Saved),
        ("other", false, Outcome::Gone),
    ] {
        let mut api = Script {
            replies: [
                (
                    p.runs_path(),
                    json!({"workflow_runs":[run(branch,"in_progress","")]}),
                ),
                (p.caches_path(), caches(saved)),
            ]
            .into(),
        };
        assert_eq!(
            poll(Duration::ZERO, Duration::ZERO, || p.state(&mut api)).unwrap(),
            expected
        );
    }
}
#[test]
fn a_running_producer_without_a_cache_times_out() {
    assert_eq!(
        poll(Duration::ZERO, Duration::ZERO, || Ok((false, true))).unwrap(),
        Outcome::TimedOut
    );
}
#[test]
fn unavailable_api_permission_is_an_error_for_the_soft_step() {
    struct Denied;
    impl Api for Denied {
        fn request(
            &mut self,
            _: &str,
            _: &str,
            _: Option<&Value>,
            _: Option<&str>,
        ) -> std::result::Result<Response, Failed> {
            Ok(Response {
                status: 403,
                headers: vec![],
                body: r#"{"message":"Resource not accessible by integration"}"#.into(),
            })
        }
    }
    assert!(
        producer()
            .state(&mut Denied)
            .unwrap_err()
            .to_string()
            .contains("403")
    );
}
