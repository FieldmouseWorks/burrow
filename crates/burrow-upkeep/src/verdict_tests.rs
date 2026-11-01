//! What each reviewer last said of a PR's head, read back from the reviews
//! the steward posted for it.

use serde_json::{Value, json};

use super::*;

const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const OLD: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
/// The steward, as REST gives its login (Pass::me).
const ME: &str = "steward[bot]";
const T0: &str = "2026-10-24T00:00:00Z";
const T1: &str = "2026-10-24T01:00:00Z";

/// The steward's App, as GraphQL gives its posts' author (no "[bot]").
fn steward() -> Value {
    json!({"login": "steward", "__typename": "Bot"})
}

/// A review by `author` on `on` at `at`, its body ending with `mark`.
fn review(author: Value, mark: &str, on: &str, at: &str) -> Review {
    Review {
        author,
        body: Some(format!(
            "**Rival review** of aaaaaaa: 2 findings.\n\n{mark}"
        )),
        commit: Some(Commit { oid: on.into() }),
        submitted_at: Some(at.into()),
    }
}

/// The steward's review for `reviewer` with `k` findings, as post.rs marks
/// it: of the head on main, shown the description the PR has.
fn posted(reviewer: &str, k: usize, at: &str) -> Review {
    review(
        steward(),
        &mark(reviewer, HEAD, "main", k, Some("4f2a")),
        HEAD,
        at,
    )
}

/// The PR at HEAD on main, its description's fingerprint 4f2a.
fn asked(reviews: Vec<Review>) -> Asked {
    Asked {
        head: HEAD.into(),
        base: "main".into(),
        description: "4f2a".into(),
        reviewers: vec!["rival".into(), "codex".into()],
        reviews,
    }
}

/// What the reviewer said: its verdict, when, and its rounds.
fn of(a: &Asked, reviewer: &str) -> (Verdict, f64, u64) {
    let said = said(ME, a).unwrap();
    let s = &said[reviewer];
    (s.verdict, s.at, s.rounds)
}

fn at(t: &str) -> f64 {
    crate::ts(t).unwrap()
}

#[test]
fn only_the_stewards_own_reviews_count() {
    // Anyone can write the mark: a user, even one whose login is the App's
    // name, which GraphQL gives the App's bot too, without "[bot]"; another
    // App; an account since deleted.
    let clean = mark("rival", HEAD, "main", 0, Some("4f2a"));
    let forged = review(
        json!({"login": "mallory", "__typename": "User"}),
        &clean,
        HEAD,
        T1,
    );
    let namesake = review(
        json!({"login": "steward", "__typename": "User"}),
        &clean,
        HEAD,
        T1,
    );
    let other = review(
        json!({"login": "rival", "__typename": "Bot"}),
        &clean,
        HEAD,
        T1,
    );
    let ghost = review(Value::Null, &clean, HEAD, T1);
    let a = asked(vec![posted("rival", 2, T0), forged, namesake, other, ghost]);
    assert_eq!(of(&a, "rival"), (Verdict::Findings, at(T0), 1));
    // Each reviewer's own marks: codex has none.
    assert_eq!(of(&a, "codex"), (Verdict::None, 0.0, 0));
    let said = said(ME, &a).unwrap();
    assert_eq!(said.keys().collect::<Vec<_>>(), ["codex", "rival"]);
}

#[test]
fn a_verdict_is_on_this_head_on_this_base_shown_this_description() {
    let a = asked(vec![posted("rival", 2, T0)]);
    assert_eq!(of(&a, "rival").0, Verdict::Findings);
    // Another head: the PR moved on, and its findings are still a round.
    let moved = Asked {
        head: OLD.into(),
        ..asked(vec![posted("rival", 2, T0)])
    };
    assert_eq!(of(&moved, "rival"), (Verdict::None, 0.0, 1));
    // A review on another commit than the one its mark names, either way.
    let elsewhere = review(
        steward(),
        &mark("rival", HEAD, "main", 0, Some("4f2a")),
        OLD,
        T0,
    );
    assert_eq!(
        of(&asked(vec![elsewhere]), "rival"),
        (Verdict::None, 0.0, 0)
    );
    let misnamed = review(
        steward(),
        &mark("rival", OLD, "main", 0, Some("4f2a")),
        HEAD,
        T0,
    );
    assert_eq!(of(&asked(vec![misnamed]), "rival"), (Verdict::None, 0.0, 0));
    // Retargeted, the same head is another change: what was said on main
    // doesn't count.
    let retargeted = Asked {
        base: "release".into(),
        ..asked(vec![posted("rival", 2, T0)])
    };
    assert_eq!(of(&retargeted, "rival"), (Verdict::None, 0.0, 1));
    // A description edited since (a reason written in after a clean review,
    // for CI) has had nobody weigh it, and a review whose mark doesn't say
    // what it was shown is no verdict at all.
    let edited = Asked {
        description: "9e1c".into(),
        ..asked(vec![posted("rival", 2, T0)])
    };
    assert_eq!(of(&edited, "rival"), (Verdict::None, 0.0, 1));
    let unsaid = review(steward(), &mark("rival", HEAD, "main", 2, None), HEAD, T0);
    assert_eq!(of(&asked(vec![unsaid]), "rival"), (Verdict::None, 0.0, 1));
}

#[test]
fn the_newest_says_it_and_every_review_with_findings_is_a_round() {
    let earlier = review(
        steward(),
        &mark("rival", OLD, "main", 3, Some("4f2a")),
        OLD,
        T0,
    );
    let found = posted("rival", 2, T0);
    let cleared = posted("rival", 0, T1);
    // Listed in any order: the newest by its time.
    for reviews in [
        vec![earlier, found, cleared],
        vec![posted("rival", 0, T1), posted("rival", 2, T0)],
    ] {
        let n = reviews.len() as u64 - 1;
        assert_eq!(of(&asked(reviews), "rival"), (Verdict::Clean, at(T1), n));
    }
    // Two at the same time: the later listed.
    let a = asked(vec![posted("rival", 0, T0), posted("rival", 1, T0)]);
    assert_eq!(of(&a, "rival"), (Verdict::Findings, at(T0), 1));
    // One with no time is at 0, before any other.
    let mut blank = posted("rival", 1, T0);
    blank.submitted_at = Some(String::new());
    assert_eq!(
        of(&asked(vec![blank]), "rival"),
        (Verdict::Findings, 0.0, 1)
    );
    let mut untimed = posted("rival", 1, T0);
    untimed.submitted_at = None;
    assert_eq!(
        of(&asked(vec![untimed]), "rival"),
        (Verdict::Findings, 0.0, 1)
    );
    let mut later = posted("rival", 1, T0);
    later.submitted_at = None;
    let a = asked(vec![posted("rival", 0, T0), later]);
    assert_eq!(of(&a, "rival"), (Verdict::Clean, at(T0), 1));
}

#[test]
fn a_mark_that_isnt_one_is_no_review() {
    let with = |said: &str| {
        let body = format!("<!-- rival-review:v1 {said} -->");
        of(&asked(vec![review(steward(), &body, HEAD, T0)]), "rival")
    };
    let on = format!(r#""commit_sha": "{HEAD}", "base": "main", "description": "4f2a""#);
    // Its findings as a number, a fraction cut off, or a number's text;
    // none at all is none found.
    assert_eq!(
        with(&format!("{{{on}, \"findings\": 1.5}}")),
        (Verdict::Findings, at(T0), 1)
    );
    assert_eq!(
        with(&format!("{{{on}, \"findings\": 0.5}}")),
        (Verdict::Clean, at(T0), 0)
    );
    assert_eq!(
        with(&format!("{{{on}, \"findings\": \" 2 \"}}")),
        (Verdict::Findings, at(T0), 1)
    );
    assert_eq!(with(&format!("{{{on}}}")), (Verdict::Clean, at(T0), 0));
    // Anything else, or a mark that isn't JSON, isn't counted at all.
    for bad in ["\"some\"", "null", "true", "[1]"] {
        assert_eq!(
            with(&format!("{{{on}, \"findings\": {bad}}}")),
            (Verdict::None, 0.0, 0),
            "{bad}"
        );
    }
    assert_eq!(
        with(&format!("{{{on}, \"findings\": 2,}}")),
        (Verdict::None, 0.0, 0)
    );
    // A head or base that isn't text names none.
    let numbered = r#"{"commit_sha": 7, "base": "main", "description": "4f2a", "findings": 1}"#;
    assert_eq!(with(numbered), (Verdict::None, 0.0, 1));
    // A mark ends where its JSON does, whatever follows it on its line.
    let followed = format!(
        "{} then {{x}} -->",
        mark("rival", HEAD, "main", 0, Some("4f2a"))
    );
    assert_eq!(
        of(
            &asked(vec![review(steward(), &followed, HEAD, T0)]),
            "rival"
        ),
        (Verdict::Clean, at(T0), 0)
    );
    // The first mark decides, and a mark is on one line.
    let first = format!(
        "<!-- rival-review:v1 {{oops}} -->\n{}",
        mark("rival", HEAD, "main", 0, Some("4f2a"))
    );
    assert_eq!(
        of(&asked(vec![review(steward(), &first, HEAD, T0)]), "rival"),
        (Verdict::None, 0.0, 0)
    );
    let split = format!("<!-- rival-review:v1 {{{on},\n\"findings\": 0}} -->");
    assert_eq!(
        of(&asked(vec![review(steward(), &split, HEAD, T0)]), "rival"),
        (Verdict::None, 0.0, 0)
    );
    // A reviewer's name is matched as itself, whatever it holds.
    let a = Asked {
        reviewers: vec!["r.v".into()],
        ..asked(vec![posted("rxv", 1, T0)])
    };
    assert_eq!(of(&a, "r.v"), (Verdict::None, 0.0, 0));
    // Another reviewer's mark, even one whose name ends with this one's.
    let theirs = mark("arival", HEAD, "main", 0, Some("4f2a"));
    let a = Asked {
        reviewers: vec!["rival".into(), "arival".into()],
        ..asked(vec![review(steward(), &theirs, HEAD, T0)])
    };
    assert_eq!(of(&a, "rival"), (Verdict::None, 0.0, 0));
    assert_eq!(of(&a, "arival"), (Verdict::Clean, at(T0), 0));
    // A review with no body says nothing.
    let mut empty = posted("rival", 1, T0);
    empty.body = None;
    assert_eq!(of(&asked(vec![empty]), "rival"), (Verdict::None, 0.0, 0));
}

#[test]
fn a_time_that_isnt_one_stops_it() {
    let mut odd = posted("rival", 1, T0);
    odd.submitted_at = Some("yesterday".into());
    let why = said(ME, &asked(vec![odd])).unwrap_err();
    assert!(why.contains("GitHub's time \"yesterday\""), "{why}");
}

#[test]
fn the_pr_is_read_as_graphql_gives_it_and_a_reviewer_is_a_plain_word() {
    let a: Asked = serde_json::from_value(json!({
        "head": HEAD, "base": "main", "description": "4f2a", "reviewers": ["rival"],
        "reviews": [
            {"author": steward(), "state": "COMMENTED", "body": mark("rival", HEAD, "main", 1, Some("4f2a")),
             "commit": {"oid": HEAD}, "submittedAt": T0},
            {"author": null, "state": "COMMENTED", "body": null, "commit": null, "submittedAt": null},
        ],
    }))
    .unwrap();
    assert_eq!(a.wrong(), None);
    assert_eq!(of(&a, "rival"), (Verdict::Findings, at(T0), 1));
    let out = serde_json::to_value(said(ME, &a).unwrap()).unwrap();
    assert_eq!(
        out,
        json!({"rival": {"verdict": "findings", "at": at(T0), "rounds": 1}})
    );
    // The reviewer's name goes into the mark it's read by.
    for name in ["Rival", "", "ri-val", "rival2", ".*"] {
        let a = Asked {
            reviewers: vec!["rival".into(), name.into()],
            ..asked(vec![])
        };
        assert_eq!(a.wrong(), Some(format!("{name:?} isn't a reviewer's name")));
    }
    // What isn't the PR as the pass gives it.
    let extra = json!({"head": HEAD, "base": "main", "description": "4f2a", "reviewers": [], "reviews": [], "body": "x"});
    assert!(serde_json::from_value::<Asked>(extra).is_err());
}

#[test]
fn a_review_covers_only_the_description_it_was_shown() {
    assert!(covers(Some("4f2a"), "4f2a"));
    assert!(!covers(Some("9e1c"), "4f2a"));
    assert!(!covers(None, "4f2a"));
}
