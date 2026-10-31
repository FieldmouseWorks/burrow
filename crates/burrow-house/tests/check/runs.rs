//! Runs overlap: every push and every edit starts one, and the newest status
//! wins. A run that read the PR before an edit can post after the edit's own
//! run; reading the PR again after it posts, it posts the verdict for the PR
//! as it is now, so the last word is the newest PR's, whichever way it goes.

use crate::*;

fn new_tool() -> Vec<serde_json::Value> {
    vec![change("added", "tools/new_tool.py", 120, 0)]
}

fn rust() -> Vec<serde_json::Value> {
    vec![change("modified", "src/main.rs", 1, 1)]
}

fn ours(state: &'static str) -> (String, &'static str, &'static str) {
    ("4ead".into(), state, "this run")
}

// The reason taken out after this run read the PR: the edit's run posts its
// failure first, then this one its old success, and then, having read the
// PR again, failure. Its files are read once: the head and base held.
#[test]
fn an_edit_meanwhile() {
    let o = case(new_tool())
        .body(REASON)
        .change(
            1,
            Change {
                body: Some("Adds a tool."),
                others: vec![("4ead", "failure")],
                ..Change::default()
            },
        )
        .run();
    assert_eq!(o.result, Ok(true));
    assert_eq!(
        said(&o.posted, "rust-first"),
        [
            ("4ead".into(), "failure", "another run"),
            ours("success"),
            ours("failure")
        ]
    );
    assert_eq!(
        o.calls.iter().filter(|c| c.starts_with("compare")).count(),
        1
    );
    // The reason put in: the last word is success.
    let o = case(new_tool())
        .body("Adds a tool.")
        .change(
            1,
            Change {
                body: Some(REASON),
                others: vec![("4ead", "success")],
                ..Change::default()
            },
        )
        .run();
    assert_eq!(
        said(&o.posted, "rust-first")[1..],
        [ours("failure"), ours("success")]
    );
    // With nothing to judge an edit changes nothing, but it's judged again
    // all the same.
    let o = case(rust())
        .change(
            1,
            Change {
                body: Some("edited"),
                ..Change::default()
            },
        )
        .run();
    assert_eq!(
        said(&o.posted, "rust-first"),
        [ours("success"), ours("success")]
    );
}

// A run posts only on the head its event named, where its own check keeps
// the head pending while it works: after a push, the new head is its own
// run's to judge, and this one stops. So does a run whose head the PR has
// already left (a re-run of an old one): it posts nothing.
#[test]
fn a_new_head() {
    let o = case(rust())
        .change(
            1,
            Change {
                head: Some("5ead"),
                ..Change::default()
            },
        )
        .compare("ba5e", "5ead", new_tool())
        .run();
    assert_eq!(o.result, Ok(true));
    assert_eq!(said(&o.posted, "rust-first"), [ours("success")]);
    assert!(o.out.contains("no longer 4ead"), "{}", o.out);
    let mut c = case(new_tool()).compare("ba5e", "5ead", rust());
    c.pr_head = "5ead";
    let o = c.run();
    assert!(
        o.result == Ok(true) && o.posted.is_empty(),
        "{:#?}",
        o.posted
    );
    assert!(
        !o.calls.iter().any(|c| c.starts_with("compare")),
        "{:?}",
        o.calls
    );
}

// A new base: the diff it judges is the new one, the comparison of the new
// base and its head.
#[test]
fn a_new_base() {
    let o = case(new_tool())
        .change(
            1,
            Change {
                base: Some(("de5e", "dev")),
                ..Change::default()
            },
        )
        .compare("de5e", "4ead", rust())
        .run();
    assert_eq!(
        said(&o.posted, "rust-first"),
        [ours("failure"), ours("success")]
    );
    let compares: Vec<_> = o
        .calls
        .iter()
        .filter(|c| c.starts_with("compare"))
        .collect();
    assert_eq!(compares, ["compare ba5e...4ead", "compare de5e...4ead"]);
}

// A PR that changes every time it's read: after ten looks it fails, until
// it's edited again.
#[test]
fn restless() {
    let mut c = case(new_tool()).body(REASON);
    c.restless = true;
    let o = c.run();
    let mut expected = vec![ours("success"); 10];
    expected.push(ours("failure"));
    assert_eq!(said(&o.posted, "rust-first"), expected);
    assert!(
        o.posted
            .last()
            .unwrap()
            .status
            .description
            .contains("kept changing")
    );
    assert!(o.out.contains("::error"), "{}", o.out);
    // Every rule gets it, each once.
    assert_eq!(o.posted.len(), 11 * RULES.len());
}
