use super::*;
use burrow_github::Failed;
use burrow_github::fake::{Fake, ok};

const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const T: u64 = 1_800_000_000;

fn cfg(dir: &Path) -> Config {
    Config {
        repos: vec!["sam/r".into()],
        per_day: 3,
        state_dir: dir.to_owned(),
        held: vec!["hold".into(), "waiting: release age".into()],
    }
}

fn connector() -> Value {
    json!({"login": "chatgpt-codex-connector", "__typename": "Bot"})
}

fn eyes(who: Value, at: &str) -> Value {
    json!([{"content": "EYES", "reactors": {"edges": [{"reactedAt": at, "node": who}]}}])
}

/// The PR as GitHub gives it: open, ready, at HEAD, nothing said of it.
fn pr() -> Value {
    json!({
        "state": "OPEN", "isDraft": false, "headRefOid": HEAD,
        "labels": {"pageInfo": {"hasNextPage": false}, "nodes": []},
        "reactionGroups": [],
        "reviewThreads": {"pageInfo": {"hasNextPage": false}, "nodes": []},
        "reviews": {"pageInfo": {"hasPreviousPage": false}, "nodes": []},
        "comments": {"pageInfo": {"hasPreviousPage": false}, "nodes": []},
    })
}

/// The connector's summary comment for HEAD, at `at`.
fn summary(status: &str, findings: impl Into<Option<bool>>, at: &str) -> Value {
    let findings = findings.into();
    let state = json!({"code_review": {"commit_sha": HEAD, "status": status, "has_any_findings": findings}});
    json!({"author": connector(), "body": format!("<!-- codex-review-state:v1 {state} -->"), "updatedAt": at, "reactionGroups": []})
}

fn github(pr: Value, post: u16) -> Fake {
    let mut gh = Fake::default();
    gh.on(
        "POST",
        "graphql",
        vec![ok(
            200,
            json!({"data": {"repository": {"pullRequest": pr}}}),
            &[],
        )],
    );
    gh.on(
        "POST",
        "repos/sam/r/issues/5/comments",
        vec![ok(post, json!({"id": 1}), &[])],
    );
    gh
}

/// The comments it posted.
fn posted(gh: &Fake) -> Vec<Value> {
    gh.asked("POST")
        .into_iter()
        .filter(|a| a.1.ends_with("/comments"))
        .map(|a| a.2.clone().unwrap())
        .collect()
}

fn record(dir: &Path) -> Vec<Asked> {
    load(&dir.join("asked.json")).unwrap().asked
}

// A head the connector hasn't said anything of gets the bare ask, the body
// exactly `@codex review`, recorded before it went.
#[test]
fn asks_bare_once_a_head() {
    let dir = tempfile::tempdir().unwrap();
    let mut gh = github(pr(), 201);
    assert_eq!(
        ask(&mut gh, &cfg(dir.path()), T, "sam/r", 5, HEAD),
        Outcome::Posted { at: T }
    );
    assert_eq!(posted(&gh), vec![json!({"body": "@codex review"})]);
    assert_eq!(ASK, "@codex review");
    assert_eq!(
        record(dir.path()),
        vec![Asked {
            repo: "sam/r".into(),
            pr: 5,
            head: HEAD.into(),
            said: 0,
            at: T
        }]
    );
    // Asked again on the same head, however the steward comes back: the
    // record says so, and nothing more is posted.
    let mut gh = github(pr(), 201);
    assert_eq!(
        ask(&mut gh, &cfg(dir.path()), T + 7200, "sam/r", 5, HEAD),
        Outcome::Asked { at: T }
    );
    assert!(posted(&gh).is_empty());
    // A new head is another ask.
    let other = "b".repeat(40);
    let mut moved = pr();
    moved["headRefOid"] = json!(other);
    let mut gh = github(moved, 201);
    assert_eq!(
        ask(&mut gh, &cfg(dir.path()), T + 60, "sam/r", 5, &other),
        Outcome::Posted { at: T + 60 }
    );
}

// Nothing is posted on a repo it isn't given, or a PR that's closed, a
// draft, held, waiting on its release age, or at another head: GitHub isn't
// even asked for an unlisted repo.
#[test]
fn never_on_unlisted_held_or_draft() {
    let dir = tempfile::tempdir().unwrap();
    let mut gh = Fake::default();
    let out = ask(&mut gh, &cfg(dir.path()), T, "mallory/r", 5, HEAD);
    assert!(
        matches!(out, Outcome::Declined { ref why } if why.contains("isn't a repo the owner asks in")),
        "{out:?}"
    );
    assert!(gh.asked.is_empty());

    let label = |l: &str| {
        let mut p = pr();
        p["labels"]["nodes"] = json!([{"name": l}]);
        p
    };
    let mut cases = vec![
        ("closed", {
            let mut p = pr();
            p["state"] = json!("CLOSED");
            p
        }),
        ("draft", {
            let mut p = pr();
            p["isDraft"] = json!(true);
            p
        }),
        ("labelled hold", label("hold")),
        (
            "labelled waiting: release age",
            label("waiting: release age"),
        ),
        ("its head is bbbb", {
            let mut p = pr();
            p["headRefOid"] = json!("b".repeat(40));
            p
        }),
        ("no such PR", Value::Null),
    ];
    for (why, p) in cases.drain(..) {
        let mut gh = github(p, 201);
        let out = ask(&mut gh, &cfg(dir.path()), T, "sam/r", 5, HEAD);
        assert!(
            matches!(out, Outcome::Declined { why: ref w } if w.contains(why)),
            "{why}: {out:?}"
        );
        assert!(posted(&gh).is_empty(), "{why}");
    }
    assert!(record(dir.path()).is_empty());
}

// A head the connector is reviewing, or found clean, isn't asked for; nor is
// one with its 👀 on an ask it has in hand, or one whose findings are still
// open. Findings all answered without a push are asked once for that word,
// and again once it has spoken again.
#[test]
#[expect(clippy::too_many_lines, reason = "legacy test")]
fn only_when_unreviewed_or_answered() {
    let dir = tempfile::tempdir().unwrap();
    let with = |comments: Value, threads: Value| {
        let mut p = pr();
        p["comments"]["nodes"] = comments;
        p["reviewThreads"]["nodes"] = threads;
        p
    };
    let running = with(
        json!([summary("running", false, "2026-01-01T00:00:00Z")]),
        json!([]),
    );
    let clean = with(
        json!([summary("completed", false, "2026-01-01T00:00:00Z")]),
        json!([]),
    );
    let open = with(
        json!([summary("completed", true, "2026-01-01T00:00:00Z")]),
        json!([{"isResolved": false}]),
    );
    let mut ask_in_hand = pr();
    ask_in_hand["comments"]["nodes"] = json!([{"author": {"login": "sam", "__typename": "User"}, "body": "@codex review", "updatedAt": "2026-01-01T00:00:00Z", "reactionGroups": eyes(connector(), "2026-01-01T00:00:00Z")}]);
    let mut on_pr = pr();
    on_pr["reactionGroups"] = eyes(connector(), "2026-01-01T00:00:00Z");
    for (why, p) in [
        ("is reviewing", running),
        ("found nothing", clean),
        ("aren't all answered", open),
        ("👀", ask_in_hand),
        ("👀", on_pr),
    ] {
        let mut gh = github(p, 201);
        let out = ask(&mut gh, &cfg(dir.path()), T, "sam/r", 5, HEAD);
        assert!(
            matches!(out, Outcome::Declined { why: ref w } if w.contains(why)),
            "{why}: {out:?}"
        );
        assert!(posted(&gh).is_empty(), "{why}");
    }
    // Nor is the connector's 👀 from before its last post on the PR (here
    // its findings, all answered since): that ask was answered.
    let mut answered_since = with(
        json!([summary("completed", true, "2026-01-01T01:00:00Z")]),
        json!([{"isResolved": true}]),
    );
    answered_since["reactionGroups"] = eyes(connector(), "2026-01-01T00:00:00Z");
    let dir2 = tempfile::tempdir().unwrap();
    assert_eq!(
        ask(
            &mut github(answered_since, 201),
            &cfg(dir2.path()),
            T,
            "sam/r",
            5,
            HEAD
        ),
        Outcome::Posted { at: T }
    );
    // Someone else's 👀 is no ask in hand.
    let mut theirs = pr();
    theirs["reactionGroups"] = eyes(
        json!({"login": "sam", "__typename": "User"}),
        "2026-01-01T00:00:00Z",
    );
    assert_eq!(
        ask(
            &mut github(theirs, 201),
            &cfg(dir.path()),
            T,
            "sam/r",
            5,
            HEAD
        ),
        Outcome::Posted { at: T }
    );

    let dir = tempfile::tempdir().unwrap();
    let answered = with(
        json!([summary("completed", true, "2026-01-01T00:00:00Z")]),
        json!([{"isResolved": true}]),
    );
    assert_eq!(
        ask(
            &mut github(answered.clone(), 201),
            &cfg(dir.path()),
            T,
            "sam/r",
            5,
            HEAD
        ),
        Outcome::Posted { at: T }
    );
    assert_eq!(
        ask(
            &mut github(answered, 201),
            &cfg(dir.path()),
            T + 60,
            "sam/r",
            5,
            HEAD
        ),
        Outcome::Asked { at: T }
    );
    let again = with(
        json!([summary("completed", true, "2026-01-01T01:00:00Z")]),
        json!([{"isResolved": true}]),
    );
    assert_eq!(
        ask(
            &mut github(again, 201),
            &cfg(dir.path()),
            T + 120,
            "sam/r",
            5,
            HEAD
        ),
        Outcome::Posted { at: T + 120 }
    );
}

// A summary that ended with no word on findings (burrow#1321: Conary#511's
// said completed, its findings null) is nothing said of the head: it's
// asked, once, keyed as a head Codex has said nothing of.
#[test]
fn a_review_ended_with_no_word_is_asked() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = pr();
    p["comments"]["nodes"] = json!([summary("completed", None, "2026-01-01T00:00:00Z")]);
    let mut gh = github(p.clone(), 201);
    assert_eq!(
        ask(&mut gh, &cfg(dir.path()), T, "sam/r", 5, HEAD),
        Outcome::Posted { at: T }
    );
    assert_eq!(posted(&gh).len(), 1);
    let asked = record(dir.path());
    assert_eq!((asked.len(), asked[0].said), (1, 0));
    assert_eq!(
        ask(
            &mut github(p, 201),
            &cfg(dir.path()),
            T + 60,
            "sam/r",
            5,
            HEAD
        ),
        Outcome::Asked { at: T }
    );
}

// The day's cap: per_day asks in any 24 hours, then none until the oldest
// is a day old.
#[test]
fn a_daily_cap() {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..3u64 {
        let head = format!("{i:0>40}");
        let mut p = pr();
        p["headRefOid"] = json!(head);
        assert_eq!(
            ask(
                &mut github(p, 201),
                &cfg(dir.path()),
                T + i,
                "sam/r",
                5,
                &head
            ),
            Outcome::Posted { at: T + i }
        );
    }
    let mut gh = github(pr(), 201);
    let out = ask(&mut gh, &cfg(dir.path()), T + 10, "sam/r", 5, HEAD);
    assert!(
        matches!(out, Outcome::Declined { ref why } if why.contains("day's cap")),
        "{out:?}"
    );
    assert!(posted(&gh).is_empty());
    assert_eq!(
        ask(
            &mut github(pr(), 201),
            &cfg(dir.path()),
            T + DAY,
            "sam/r",
            5,
            HEAD
        ),
        Outcome::Posted { at: T + DAY }
    );
}

// GitHub's no takes the record back, so a later pass can ask; a lost
// answer keeps it, since the comment may be up.
#[test]
fn a_refusal_unrecords_a_lost_answer_doesnt() {
    let dir = tempfile::tempdir().unwrap();
    let out = ask(
        &mut github(pr(), 403),
        &cfg(dir.path()),
        T,
        "sam/r",
        5,
        HEAD,
    );
    assert!(matches!(out, Outcome::Failed { .. }), "{out:?}");
    assert!(record(dir.path()).is_empty());

    let mut gh = github(pr(), 201);
    gh.answers.retain(|a| a.0 != "POST" || a.1 == "graphql");
    gh.on(
        "POST",
        "repos/sam/r/issues/5/comments",
        vec![Err(Failed {
            why: "connection reset".into(),
            transient: true,
        })],
    );
    let out = ask(&mut gh, &cfg(dir.path()), T, "sam/r", 5, HEAD);
    assert!(
        matches!(out, Outcome::Failed { ref why } if why.contains("kept as asked")),
        "{out:?}"
    );
    assert_eq!(posted(&gh).len(), 1, "a POST isn't asked twice");
    assert_eq!(record(dir.path()).len(), 1);

    // A rate limit is GitHub's no, however it's said: nothing went up, so
    // the record goes and a later pass asks.
    let dir = tempfile::tempdir().unwrap();
    let mut gh = github(pr(), 201);
    gh.answers.retain(|a| a.0 != "POST" || a.1 == "graphql");
    gh.on(
        "POST",
        "repos/sam/r/issues/5/comments",
        vec![ok(
            429,
            json!({"message": "slow down"}),
            &[("retry-after", "60")],
        )],
    );
    let out = ask(&mut gh, &cfg(dir.path()), T, "sam/r", 5, HEAD);
    assert!(
        matches!(out, Outcome::Failed { ref why } if !why.contains("kept as asked")),
        "{out:?}"
    );
    assert!(record(dir.path()).is_empty());
}

#[test]
fn only_an_ask_exits_zero() {
    // Conary's flow counts a head as asked on exit 0 alone: a no that may
    // not last (the day's cap) must leave it to ask again.
    assert_eq!(Outcome::Posted { at: 1 }.code(), 0);
    assert_eq!(Outcome::Asked { at: 1 }.code(), 0);
    assert_ne!(
        Outcome::Declined {
            why: "the day's cap".into()
        }
        .code(),
        0
    );
    assert_ne!(
        Outcome::Failed {
            why: "refused".into()
        }
        .code(),
        0
    );
    assert_ne!(
        Outcome::Declined { why: String::new() }.code(),
        Outcome::Failed { why: String::new() }.code()
    );
}
