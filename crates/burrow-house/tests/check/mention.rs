//! The connector's mention: Codex's GitHub connector reads a PR whose title
//! or body writes it, even in backticks, as a task for itself, answers that
//! the repo needs an environment, and posts no review of the head. So a PR
//! that writes it fails, and no body line passes it: the words have to go.

use crate::*;

fn rust() -> Vec<serde_json::Value> {
    vec![change("modified", "src/main.rs", 1, 1)]
}

// The PRs that met it (Conary#470 and three more): the mention in backticks
// in a sentence of the body. None got a review until the steward asked.
#[test]
fn in_the_body() {
    let body = "When a head sits unreviewed, `Step::AskReview` posted `@codex review` as the flow's App.\n\nFixes #469\n";
    let c = case(rust()).body(body);
    let error = fails(&c, "mention");
    assert!(error.contains("PR's body writes '@codex'"), "{error}");
    let o = c.run();
    let p = o
        .posted
        .iter()
        .find(|p| p.status.context == "house/mention (#7)")
        .unwrap();
    // The author's text stays out of the status and the log.
    assert!(!p.status.description.contains("AskReview"), "{p:?}");
    assert!(!o.out.contains("AskReview"), "{}", o.out);
    // A reason in the body doesn't take it back.
    fails(
        &c.clone()
            .body(&format!("{body}Why a mention: the PR is about the ask\n")),
        "mention",
    );
    // In a code block, or a reply's quote, it's still written.
    fails(&case(rust()).body("```\n@Codex review\n```\n"), "mention");
    fails(&case(rust()).body("> @codex review\r\n"), "mention");
}

#[test]
fn in_the_title() {
    let error = fails(
        &case(rust()).title("review-ask: post @codex review as the owner"),
        "mention",
    );
    assert!(error.contains("PR's title writes"), "{error}");
}

// What burrow#1254's body wrote in its place, and text that only looks
// like it.
#[test]
fn in_other_words() {
    for body in [
        "Ask with the connector's bare review mention, once a head.\n",
        "Codex reviews every push through its GitHub connector.\n",
        "Mail it to ops@codex.example.\n",
        "Thanks, @codex-fan.\n",
    ] {
        passes(&case(rust()).body(body));
    }
}

#[test]
fn a_forks() {
    let mut c = case(rust()).body("cc @codex\n");
    c.fork = Fork::Yes;
    fails(&c, "mention");
}

// Taking it out of the body (an edit) passes it.
#[test]
fn edited() {
    let o = case(rust())
        .body("Asks with `@codex review`.\n")
        .change(
            1,
            Change {
                body: Some("Asks with the connector's bare review mention.\n"),
                ..Change::default()
            },
        )
        .run();
    assert_eq!(o.result, Ok(true));
    assert_eq!(
        said(&o.posted, "mention"),
        [
            ("4ead".into(), "failure", "this run"),
            ("4ead".into(), "success", "this run")
        ]
    );
}

// The mention is the one the steward's own ask starts with: if the ask
// changes, so does what this looks for.
#[test]
fn the_asks_own() {
    use burrow_house::rules::{MENTION, mentions};
    assert!(burrow_review_ask::ASK.starts_with(MENTION));
    assert!(mentions(burrow_review_ask::ASK));
}
