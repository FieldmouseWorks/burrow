use serde_json::{Value, json};

use super::{End, Ends, ci, ended, findings};

fn run(name: &str, status: &str, conclusion: Option<&str>) -> Value {
    json!({"name": name, "status": status, "conclusion": conclusion})
}

const NO_STATUS: fn() -> Value = || json!({"total_count": 0, "state": "pending", "statuses": []});

// A red leg decides at once; running legs, cancelled and skipped ones don't,
// and green needs every leg passed.
#[test]
fn the_heads_ci_verdict() {
    let v = |runs: &[Value], status: Value| ci("h", runs, &status)["verdict"].clone();
    assert_eq!(v(&[], NO_STATUS()), "none");
    assert_eq!(v(&[run("a", "queued", None)], NO_STATUS()), "running");
    assert_eq!(
        v(
            &[
                run("a", "completed", Some("success")),
                run("b", "in_progress", None)
            ],
            NO_STATUS()
        ),
        "running"
    );
    assert_eq!(
        v(
            &[
                run("a", "completed", Some("failure")),
                run("b", "in_progress", None)
            ],
            NO_STATUS()
        ),
        "red"
    );
    assert_eq!(
        v(
            &[
                run("a", "completed", Some("success")),
                run("b", "completed", Some("skipped")),
                run("c", "completed", Some("neutral"))
            ],
            NO_STATUS()
        ),
        "green"
    );
    assert_eq!(
        v(
            &[
                run("a", "completed", Some("success")),
                run("b", "completed", Some("cancelled"))
            ],
            NO_STATUS()
        ),
        "none"
    );
    for red in ["timed_out", "action_required", "startup_failure"] {
        assert_eq!(
            v(&[run("a", "completed", Some(red))], NO_STATUS()),
            "red",
            "{red}"
        );
    }
    // Commit statuses count as legs too.
    let status = |state: &str| json!({"total_count": 1, "state": state, "statuses": [{"context": "ext", "state": state}]});
    assert_eq!(
        v(&[run("a", "completed", Some("success"))], status("pending")),
        "running"
    );
    assert_eq!(
        v(&[run("a", "completed", Some("success"))], status("success")),
        "green"
    );
    assert_eq!(v(&[], status("success")), "green");
    let red = ci("h", &[run("a", "queued", None)], &status("failure"));
    assert_eq!(red["verdict"], "red");
    assert_eq!(red["failed"], json!(["status ext"]));
}

// Changes requested, a steward mark counting findings, or an unmarked
// comment review with something to say; never a clean or approving one.
#[test]
fn which_reviews_have_findings() {
    let mark =
        |k: u64| format!("x\n<!-- codex-review:v1 {{\"commit_sha\":\"h\",\"findings\":{k}}} -->");
    let reviews = [
        json!({"state": "COMMENTED", "body": mark(0)}),
        json!({"state": "APPROVED", "body": "lgtm"}),
        json!({"state": "COMMENTED", "body": ""}),
        json!({"state": "COMMENTED", "body": mark(3)}),
        json!({"state": "CHANGES_REQUESTED", "body": ""}),
        json!({"state": "COMMENTED", "body": "please rename this"}),
        json!({"state": "DISMISSED", "body": "old"}),
    ];
    assert_eq!(findings(&reviews), 3);
}

// --ci and --review lead; the rest are targets.
#[test]
fn flags_parse() {
    let args: Vec<String> = ["--review", "--ci", "o/r#1", "--ci"]
        .map(String::from)
        .into();
    let (ends, rest) = Ends::parse(&args);
    assert_eq!(ends, Ends([End::Ci, End::Review].into_iter().collect()));
    assert_eq!(rest, &args[2..]);
    let (ends, rest) = Ends::parse(&args[2..]);
    assert!(ends.any_change());
    assert_eq!(rest.len(), 2);
}

// A condition needs a PR: an issue's fingerprint has no CI to wait on.
#[test]
fn an_issue_takes_no_condition() {
    let issue = json!({"state": "open", "comments": 2});
    let ci_wait = Ends([End::Ci].into_iter().collect());
    assert!(ci_wait.baseline(&issue).is_err());
    assert_eq!(Ends::default().baseline(&issue).unwrap(), issue);
}

// Reopening is news to a conditional wait, as closing is.
#[test]
fn a_reopen_is_news() {
    let was = json!({"state": "closed", "merged": false, "ends": ["review"]});
    let now = json!({"state": "open", "merged": false});
    assert_eq!(
        ended(was.as_object().unwrap(), now.as_object().unwrap()).as_deref(),
        Some("reopened")
    );
}
