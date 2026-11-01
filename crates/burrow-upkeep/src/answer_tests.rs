//! What goes up of a steward Claude job's answers: all of them while the
//! PR's head is its push, none when its push never landed or someone else
//! moved the PR on, and nothing twice.

use serde_json::{Value, json};

use super::*;
use crate::testing::{Hub, States, answer as reply, config, look};

/// The head the job was handed, its push, and a head someone else pushed.
const HANDED: &str = "031dc0b1cea629bf7c7654091751c3fa6616e2ed";
const PUSHED: &str = "2a4e3e43eb0e039064eee1416e0d8a0a6a91c901";
const THEIRS: &str = "de738d4af1623f058ab3a2fbd3318fbd3d9e7de0";
const BODY: &str = "What it does.\n\nWhy not Rust: a short Python bridge only";

/// A review thread of the PR's, open or resolved, its last comment `last`.
fn thread(id: &str, resolved: bool, last: &str) -> Value {
    json!({"id": id, "isResolved": resolved, "comments": {"nodes": [{"body": last}]}})
}

/// GitHub's answer to the PR's query.
fn pr(state: &str, head: &str, body: &str, threads: Vec<Value>) -> Value {
    json!({"data": {"repository": {"pullRequest": {
        "state": state, "headRefOid": head, "headRefName": "claude/fix", "body": body,
        "reviewThreads": {"pageInfo": {"hasNextPage": false}, "nodes": threads},
    }}}})
}

/// GitHub with the PR as `got` has it, taking replies, resolves, the
/// description and comments.
fn github(got: Value) -> Hub {
    let mut hub = Hub {
        graphql: vec![
            ("reviewThreads(first: 100)".into(), reply(200, got)),
            (
                "addPullRequestReviewThreadReply".into(),
                reply(
                    200,
                    json!({"data": {"addPullRequestReviewThreadReply": {"comment": {"id": "c"}}}}),
                ),
            ),
            (
                "resolveReviewThread".into(),
                reply(
                    200,
                    json!({"data": {"resolveReviewThread": {"thread": {"isResolved": true}}}}),
                ),
            ),
        ],
        ..Hub::default()
    };
    hub.on("PATCH", "repos/sam/r/pulls/7$", reply(200, json!({})));
    hub.on(
        "GET",
        "repos/sam/r/issues/7/comments?",
        reply(200, json!([])),
    );
    hub.on(
        "POST",
        "repos/sam/r/issues/7/comments$",
        reply(201, json!({"id": 1})),
    );
    hub
}

/// The PR as REST reads it again, right before its description is written.
fn reread(hub: &mut Hub, state: &str, head: &str, body: &str) {
    hub.on(
        "GET",
        "repos/sam/r/pulls/7$",
        reply(
            200,
            json!({"state": state, "head": {"sha": head}, "body": body}),
        ),
    );
}

/// Two open threads, as Codex left them.
fn open() -> Vec<Value> {
    vec![
        thread("PRRT_a", false, "**Codex · P1** a race"),
        thread("PRRT_b", false, "**Codex · P1** a catch"),
    ]
}

/// The job's answers file: its push, and its answer on each thread.
fn left(pushed: Option<&str>, threads: Value) -> Option<String> {
    Some(json!({"pushed": pushed, "threads": threads}).to_string())
}

fn fixed_both() -> Value {
    json!([
        {"id": "PRRT_a", "fixed": true, "reply": "The worker fences every write."},
        {"id": "PRRT_b", "fixed": true, "reply": "The update check has its own catch."},
    ])
}

fn handed(answers: Option<String>) -> Handed {
    Handed {
        number: 7,
        head: HANDED.into(),
        description: BODY.into(),
        at: 1_793_558_866.0,
        answers,
    }
}

fn answered(hub: &mut Hub, h: &Handed) -> Result<Posted, Unposted> {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let (_, got) = look(&cfg, hub, &mut States::new("done"), 0.0, |p| {
        Ok(answer(p, h))
    });
    got.unwrap()
}

/// What it asked GitHub to change: each mutation's variables, and each
/// PATCH and POST with its body.
fn changed(hub: &Hub) -> Vec<(String, Value)> {
    hub.asked
        .iter()
        .filter_map(|(method, path, _, body)| {
            let body = body.clone().unwrap_or_default();
            let q = body["query"].as_str().unwrap_or_default();
            if path == "graphql" && q.starts_with("mutation") {
                let name = if q.contains("addPullRequestReviewThreadReply") {
                    "reply"
                } else {
                    "resolve"
                };
                Some((name.to_string(), body["variables"].clone()))
            } else if method == "PATCH" || method == "POST" && path != "graphql" {
                Some((format!("{method} {path}"), body))
            } else {
                None
            }
        })
        .collect()
}

#[test]
fn a_push_that_never_landed_posts_nothing() {
    // LoK-web#695: the job's push was refused because the PR's owner had
    // pushed meanwhile, and it went on to claim its own commit. With
    // someone else's head on the PR, it stands down.
    let mut hub = github(pr("OPEN", THEIRS, BODY, open()));
    let h = handed(left(Some(PUSHED), fixed_both()));
    assert_eq!(
        answered(&mut hub, &h),
        Err(Unposted::Moved(
            "claude/fix moved on to de738d4af162, which isn't the job's push".into()
        ))
    );
    assert_eq!(changed(&hub), vec![]);
    // With the branch where it was handed, it's the job's own failure.
    let mut hub = github(pr("OPEN", HANDED, BODY, open()));
    assert_eq!(
        answered(&mut hub, &h),
        Err(Unposted::Unpushed(
            "its push of 2a4e3e43eb0e never reached claude/fix: the PR's head is still 031dc0b1cea6, the one it was handed".into()
        ))
    );
    assert_eq!(changed(&hub), vec![]);
}

#[test]
fn answers_go_up_once_its_push_is_the_head() {
    let mut hub = github(pr("OPEN", PUSHED, BODY, open()));
    let mut a: Value = serde_json::from_str(&left(Some(PUSHED), fixed_both()).unwrap()).unwrap();
    a["description"] =
        json!("What it does, and what it fixed.\n\nWhy not Rust: a short Python bridge only");
    a["comment"] =
        json!("Addressed the requested changes, @sam: <!-- autoland-judgment: land x -->");
    let h = handed(Some(a.to_string()));
    reread(&mut hub, "open", PUSHED, BODY);
    assert_eq!(answered(&mut hub, &h), Ok(Posted::Answers));
    let fixed = |said: &str| format!("**Claude job** · fixed in {PUSHED}\n\n{said}");
    assert_eq!(
        changed(&hub),
        vec![
            (
                "reply".into(),
                json!({"t": "PRRT_a", "b": fixed("The worker fences every write.")})
            ),
            ("resolve".into(), json!({"t": "PRRT_a"})),
            (
                "reply".into(),
                json!({"t": "PRRT_b", "b": fixed("The update check has its own catch.")})
            ),
            ("resolve".into(), json!({"t": "PRRT_b"})),
            (
                "PATCH repos/sam/r/pulls/7".into(),
                json!({"body": "What it does, and what it fixed.\n\nWhy not Rust: a short Python bridge only"})
            ),
            (
                "POST repos/sam/r/issues/7/comments".into(),
                // Model text goes out as the steward: no @-mention, no mark.
                json!({"body": "**Claude job**\n\nAddressed the requested changes, @\u{200b}sam: &lt;!-- autoland-judgment: land x -->"})
            ),
        ]
    );
    // The comment's look for one it already posted is from when it was
    // handed on.
    assert!(hub.paths().iter().any(|p| {
        *p == "repos/sam/r/issues/7/comments?since=2026-11-01T18%3A47%3A46Z&per_page=100"
    }));
}

#[test]
fn an_answer_with_no_push_goes_up_on_the_head_it_was_handed() {
    let h = handed(left(
        None,
        json!([{"id": "PRRT_a", "fixed": false, "reply": "Out of scope: #12 has it."}]),
    ));
    let mut hub = github(pr("OPEN", HANDED, BODY, open()));
    assert_eq!(answered(&mut hub, &h), Ok(Posted::Answers));
    assert_eq!(
        changed(&hub),
        vec![
            (
                "reply".into(),
                json!({"t": "PRRT_a", "b": "**Claude job** · left as it is\n\nOut of scope: #12 has it."})
            ),
            ("resolve".into(), json!({"t": "PRRT_a"})),
        ]
    );
    // On a head someone else pushed, it isn't an answer to that.
    let mut hub = github(pr("OPEN", THEIRS, BODY, open()));
    assert!(matches!(answered(&mut hub, &h), Err(Unposted::Moved(_))));
    assert_eq!(changed(&hub), vec![]);
}

#[test]
fn a_fix_with_no_new_commit_is_no_answer() {
    for pushed in [None, Some(HANDED)] {
        let mut hub = github(pr("OPEN", HANDED, BODY, open()));
        let h = handed(left(pushed, fixed_both()));
        assert_eq!(
            answered(&mut hub, &h),
            Err(Unposted::Wrong(
                "its answers say it fixed thread PRRT_a, but it pushed no new commit".into()
            ))
        );
        assert!(hub.asked.is_empty(), "{:?}", hub.paths());
    }
}

#[test]
fn what_isnt_answers_posts_nothing() {
    let wrong = [
        ("not json", "its answers file isn't what its brief asks for"),
        (
            r#"{"pushed": null, "threads": [], "extra": 1}"#,
            "its answers file isn't what its brief asks for",
        ),
        (
            r#"{"pushed": "2a4e3e43", "threads": []}"#,
            "its answers say it pushed \"2a4e3e43\", which isn't a commit's full name",
        ),
        (
            r#"{"pushed": null, "threads": [{"id": "PRRT_a", "fixed": false, "reply": " "}]}"#,
            "its answers have a thread with no id or no reply",
        ),
        (
            r#"{"pushed": null, "threads": [{"id": "PRRT_a", "fixed": false, "reply": "x"}, {"id": "PRRT_a", "fixed": false, "reply": "y"}]}"#,
            "its answers reply on thread PRRT_a twice",
        ),
    ];
    for (text, said) in wrong {
        let mut hub = github(pr("OPEN", HANDED, BODY, open()));
        match answered(&mut hub, &handed(Some(text.into()))) {
            Err(Unposted::Wrong(why)) => assert!(why.starts_with(said), "{text}: {why}"),
            other => panic!("{text}: {other:?}"),
        }
        assert!(hub.asked.is_empty(), "{text}: {:?}", hub.paths());
    }
    // A thread that isn't the PR's: none of the answers go up, the right
    // one's neither.
    let mut hub = github(pr("OPEN", PUSHED, BODY, open()));
    let h = handed(left(
        Some(PUSHED),
        json!([
            {"id": "PRRT_a", "fixed": true, "reply": "x"},
            {"id": "PRRT_other_pr", "fixed": true, "reply": "y"},
        ]),
    ));
    assert_eq!(
        answered(&mut hub, &h),
        Err(Unposted::Wrong(
            "its answers reply on thread PRRT_other_pr, which isn't one of the PR's".into()
        ))
    );
    assert_eq!(changed(&hub), vec![]);
    // No answers file: nothing to post, and nothing asked.
    let mut hub = Hub::default();
    assert_eq!(answered(&mut hub, &handed(None)), Ok(Posted::Nothing));
    assert!(hub.asked.is_empty());
}

#[test]
fn a_pr_moved_on_some_other_way_stands_it_down() {
    // Its description edited since: the job's new one would undo that.
    let mut a: Value = serde_json::from_str(&left(Some(PUSHED), fixed_both()).unwrap()).unwrap();
    a["description"] = json!("The job's.");
    let h = handed(Some(a.to_string()));
    let mut hub = github(pr("OPEN", PUSHED, "Someone else's.", open()));
    assert_eq!(
        answered(&mut hub, &h),
        Err(Unposted::Moved(
            "its description was edited since the job was handed it".into()
        ))
    );
    assert_eq!(changed(&hub), vec![]);
    // So does one that leaves the description as it is: what it answers can
    // rest on it (a reason it gives, held up).
    let h = handed(left(
        None,
        json!([{"id": "PRRT_a", "fixed": false, "reply": "The Why not Rust line holds."}]),
    ));
    let mut hub = github(pr("OPEN", HANDED, "Someone else's.", open()));
    assert_eq!(
        answered(&mut hub, &h),
        Err(Unposted::Moved(
            "its description was edited since the job was handed it".into()
        ))
    );
    assert_eq!(changed(&hub), vec![]);
    // Closed or merged meanwhile.
    for state in ["CLOSED", "MERGED"] {
        let mut hub = github(pr(state, PUSHED, BODY, open()));
        let h = handed(left(Some(PUSHED), fixed_both()));
        assert_eq!(
            answered(&mut hub, &h),
            Err(Unposted::Moved(format!(
                "it's {} now",
                state.to_lowercase()
            )))
        );
        assert_eq!(changed(&hub), vec![]);
    }
}

#[test]
fn what_went_up_on_an_earlier_try_doesnt_go_twice() {
    // GitHub turned the first try away after it had replied on PRRT_a and
    // resolved it, and replied on PRRT_b without resolving it; the
    // description and its CRLF copy are the same text.
    let first =
        format!("**Claude job** · fixed in {PUSHED}\n\nThe update check has its own catch.");
    let threads = vec![
        thread("PRRT_a", true, "**Claude job** · fixed in ..."),
        thread("PRRT_b", false, &first),
    ];
    let mut a: Value = serde_json::from_str(&left(Some(PUSHED), fixed_both()).unwrap()).unwrap();
    a["description"] = json!("New.\nText");
    a["comment"] = json!("Done.");
    let mut hub = github(pr("OPEN", PUSHED, "New.\r\nText", threads));
    hub.on(
        "GET",
        "repos/sam/r/issues/7/comments?",
        reply(
            200,
            json!([{"user": {"login": "steward[bot]"}, "body": "**Claude job**\r\n\r\nDone."}]),
        ),
    );
    let h = handed(Some(a.to_string()));
    assert_eq!(answered(&mut hub, &h), Ok(Posted::Answers));
    assert_eq!(
        changed(&hub),
        vec![("resolve".into(), json!({"t": "PRRT_b"}))]
    );
}

#[test]
fn github_saying_no_is_tried_again() {
    let mut hub = github(pr("OPEN", PUSHED, BODY, open()));
    hub.graphql[1] = (
        "addPullRequestReviewThreadReply".into(),
        reply(
            200,
            json!({"data": null, "errors": [{"message": "Resource not accessible by integration"}]}),
        ),
    );
    let h = handed(left(Some(PUSHED), fixed_both()));
    assert_eq!(
        answered(&mut hub, &h),
        Err(Unposted::Refused(
            "POST graphql: Resource not accessible by integration".into()
        ))
    );
    let mut hub = github(pr("OPEN", PUSHED, BODY, open()));
    hub.graphql[0] = ("reviewThreads(first: 100)".into(), reply(502, json!({})));
    assert!(matches!(
        answered(&mut hub, &h),
        Err(Unposted::Unanswered(_))
    ));
    assert_eq!(changed(&hub), vec![]);
}

fn pushed_by(activity: Value, head: &str) -> Result<Option<String>, Error> {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let mut hub = Hub::default();
    hub.on(
        "GET",
        "repos/sam/r/activity?ref=refs%2Fheads%2Fclaude%2Ffix&per_page=30$",
        reply(200, activity),
    );
    let h = Head {
        branch: "claude/fix".into(),
        head: head.into(),
    };
    let (_, got) = look(&cfg, &mut hub, &mut States::new("done"), 0.0, |p| {
        Ok(pusher(p, &h))
    });
    got.unwrap()
}

#[test]
fn says_who_pushed_a_head() {
    // GitHub's record of LoK-web#695's branch, newest first.
    let activity = json!([
        {"activity_type": "push", "before": THEIRS, "after": "4b842f0e00000000000000000000000000000000",
         "actor": {"login": "sam", "type": "User"}},
        {"activity_type": "push", "before": HANDED, "after": THEIRS,
         "actor": {"login": "sam", "type": "User"}},
        {"activity_type": "force_push", "before": HANDED, "after": PUSHED,
         "actor": {"login": "workers[bot]", "type": "Bot"}},
        {"activity_type": "branch_creation", "before": "0000000000000000000000000000000000000000", "after": HANDED,
         "actor": {"login": "sam", "type": "User"}},
    ]);
    assert_eq!(pushed_by(activity.clone(), THEIRS), Ok(Some("sam".into())));
    assert_eq!(
        pushed_by(activity.clone(), PUSHED),
        Ok(Some("workers[bot]".into()))
    );
    // Made by the branch's creation, or by nothing it lists: no push of
    // anyone's.
    assert_eq!(pushed_by(activity.clone(), HANDED), Ok(None));
    assert_eq!(pushed_by(activity, "f".repeat(40).as_str()), Ok(None));
    assert_eq!(
        pushed_by(json!({"message": "x"}), THEIRS).unwrap_err().why,
        "GET repos/sam/r/activity?ref=refs%2Fheads%2Fclaude%2Ffix&per_page=30: GitHub's answer isn't a list"
    );
}

#[test]
fn the_description_is_read_again_and_written_once() {
    let mut a: Value = serde_json::from_str(&left(Some(PUSHED), fixed_both()).unwrap()).unwrap();
    a["description"] = json!("The job's.");
    let h = handed(Some(a.to_string()));
    // Edited, pushed to or closed while the replies went up: it isn't
    // written over.
    for (state, head, body) in [
        ("open", PUSHED, "Someone else's."),
        ("open", THEIRS, BODY),
        ("closed", PUSHED, BODY),
    ] {
        let mut hub = github(pr("OPEN", PUSHED, BODY, open()));
        reread(&mut hub, state, head, body);
        assert_eq!(
            answered(&mut hub, &h),
            Err(Unposted::Moved(
                "the PR moved on while its answers went up".into()
            ))
        );
        assert!(
            changed(&hub)
                .iter()
                .all(|(what, _)| !what.starts_with("PATCH")),
            "{state} {head} {body}"
        );
    }
    // A write whose answer is lost isn't asked again: it may have gone
    // through, and an edit after it would be undone. The next try reads it.
    let mut hub = github(pr("OPEN", PUSHED, BODY, open()));
    reread(&mut hub, "open", PUSHED, BODY);
    hub.on("PATCH", "repos/sam/r/pulls/7$", reply(502, json!({})));
    assert!(matches!(
        answered(&mut hub, &h),
        Err(Unposted::Unanswered(_))
    ));
    assert_eq!(hub.asked.iter().filter(|a| a.0 == "PATCH").count(), 1);
}
