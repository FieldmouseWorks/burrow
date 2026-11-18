//! What Codex last said of a PR's head, read from its GitHub connector's
//! summary comment and reviews.

use serde_json::{Value, json};

use super::*;

const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const OLD: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const T0: &str = "2026-10-24T00:00:00Z";
const T1: &str = "2026-10-24T01:00:00Z";

/// The connector, as GraphQL gives its posts' author (no "[bot]").
fn connector() -> Value {
    json!({"login": "chatgpt-codex-connector", "__typename": "Bot"})
}

/// Someone writing the connector's marks by hand.
fn forger() -> Value {
    json!({"login": "mallory", "__typename": "User"})
}

/// The connector's summary comment, as it keeps it, for a review of `of`.
fn summary(author: Value, of: &str, status: &str, findings: Option<bool>, at: &str) -> Comment {
    let state = json!({
        "code_review": {"commit_sha": of, "has_any_findings": findings, "has_unresolved_findings": findings, "status": status},
        "security_review": {"commit_sha": null, "has_any_findings": null, "has_unresolved_findings": null, "status": null},
    });
    Comment {
        author,
        body: Some(format!(
            "<!-- codex-pull-request-review-summary -->\n<!-- codex-review-state:v1 {state} -->\n\n## Codex Review Summary"
        )),
        updated_at: Some(at.into()),
    }
}

/// The connector's review with findings of `on`, its receipt naming `named`.
fn receipt(author: Value, on: &str, named: &str, at: &str) -> Review {
    Review {
        author,
        body: Some(format!(
            "<!-- codex-review-receipt:v1 -->\n\n### 💡 Codex Review\n\n**Reviewed commit:** `{named}`\n"
        )),
        commit: Some(Commit { oid: on.into() }),
        submitted_at: Some(at.into()),
    }
}

fn of(reviews: Vec<Review>, comments: Vec<Comment>) -> (Verdict, f64, u64) {
    let s = said(&Asked {
        head: HEAD.into(),
        reviews,
        comments,
    })
    .unwrap();
    (s.verdict, s.at, s.rounds)
}

#[test]
fn nothing_said_is_no_verdict() {
    assert_eq!(of(vec![], vec![]), (Verdict::None, 0.0, 0));
}

#[test]
fn a_finished_review_of_the_head_with_nothing_found_is_clean() {
    let at = crate::ts(T1).unwrap();
    assert_eq!(
        of(vec![], vec![summary(connector(), HEAD, "completed", Some(false), T1)]),
        (Verdict::Clean, at, 0)
    );
}

#[test]
fn a_finished_review_with_findings_is_findings() {
    let (v, _, rounds) = of(
        vec![receipt(connector(), HEAD, &HEAD[..10], T0)],
        vec![summary(connector(), HEAD, "completed", Some(true), T1)],
    );
    assert_eq!((v, rounds), (Verdict::Findings, 1));
}

#[test]
fn a_review_under_way_is_running() {
    assert_eq!(
        of(vec![], vec![summary(connector(), HEAD, "running", None, T1)]).0,
        Verdict::Running
    );
}

#[test]
fn a_clean_review_of_another_commit_says_nothing_of_the_head() {
    assert_eq!(
        of(vec![], vec![summary(connector(), OLD, "completed", Some(false), T1)]).0,
        Verdict::None
    );
}

#[test]
fn only_the_connectors_own_posts_count() {
    // The bug this guards: anyone can write the mark, so a PR's author
    // could call its own head clean.
    assert_eq!(
        of(
            vec![receipt(forger(), HEAD, &HEAD[..10], T0)],
            vec![summary(forger(), HEAD, "completed", Some(false), T1)]
        ),
        (Verdict::None, 0.0, 0)
    );
}

#[test]
fn findings_on_an_earlier_head_are_a_round_but_not_this_heads_verdict() {
    let (v, _, rounds) = of(
        vec![
            receipt(connector(), OLD, &OLD[..10], T0),
            receipt(connector(), HEAD, &HEAD[..10], T1),
        ],
        vec![],
    );
    assert_eq!((v, rounds), (Verdict::Findings, 2));
    let (v, _, rounds) = of(vec![receipt(connector(), OLD, &OLD[..10], T0)], vec![]);
    assert_eq!((v, rounds), (Verdict::None, 1));
}

#[test]
fn a_receipt_that_names_another_commit_than_its_own_isnt_the_heads() {
    assert_eq!(
        of(vec![receipt(connector(), HEAD, &OLD[..10], T0)], vec![]).0,
        Verdict::None
    );
}

#[test]
fn the_newest_word_on_the_head_wins() {
    // Findings first, then a clean look at the same head after their
    // answers were resolved and the review asked for again.
    assert_eq!(
        of(
            vec![receipt(connector(), HEAD, &HEAD[..10], T0)],
            vec![summary(connector(), HEAD, "completed", Some(false), T1)]
        )
        .0,
        Verdict::Clean
    );
}
