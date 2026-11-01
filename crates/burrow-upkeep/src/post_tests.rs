//! What gets posted from a reviewer's answer: one review on the head it
//! read, each finding a thread on the line it's about where it can be.

use serde_json::{Value, json};

use super::*;
use crate::testing::{Hub, States, answer, config, look};

const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const COMPARE: &str = "repos/sam/r/compare/main...aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa$";
const REVIEWS: &str = "repos/sam/r/pulls/7/reviews";
const THREADS: &str = "repos/sam/r/pulls/7/comments";

/// GitHub's compare: a changed file, a new one with a name git would quote,
/// a rename, a name with a space, a mode change alone, a deleted file and
/// a binary one. What a pass reads the lines a thread can go on from.
fn files() -> Value {
    json!({"files": [
        {"filename": "src/app.py", "status": "modified",
         "patch": "@@ -10,4 +10,5 @@ def f():\n     a = 1\n---- a removed line that looks like a header\n+    b = 2\n+    c = 3\n     return a"},
        {"filename": "cert-é.pem", "status": "added", "patch": "@@ -0,0 +1 @@\n+x\n\\ No newline at end of file"},
        {"filename": "config.txt", "previous_filename": "foo b/.env", "status": "renamed",
         "patch": "@@ -1,2 +1,2 @@\n x=0\n-x=1\n+x=2"},
        {"filename": "my notes.txt", "status": "modified", "patch": "@@ -1 +1 @@\n-old\n+new\r"},
        {"filename": "start.sh", "status": "changed"},
        {"filename": "gone.txt", "status": "removed", "patch": "@@ -1 +0,0 @@\n-bye"},
        {"filename": "logo.png", "status": "modified"},
    ]})
}

/// The look a pass hands over, `answer` its reviewer's.
fn of(reviewer: &str, name: &str, answer: Value) -> Look {
    Look {
        number: 7,
        head: HEAD.into(),
        base: "main".into(),
        reviewer: reviewer.into(),
        name: name.into(),
        answer,
        description: Some("4f2a".into()),
    }
}

fn three() -> Value {
    json!({"findings": [
        {"path": "src/app.py", "line": 99, "severity": "P0", "title": "ping @evil",
         "body": "bad <!-- rival-review:v1 {} -->", "covers": "every caller, @evil's too"},
        {"path": "gone.txt", "line": 1, "severity": "P2", "title": "t", "body": "b", "covers": " \n"},
        {"path": "nope.py", "line": 1, "severity": "PX", "title": "t2", "body": "b2"},
    ], "summary": "s"})
}

/// GitHub with the compare above, taking reviews and threads.
fn github() -> Hub {
    let mut hub = Hub::default();
    hub.on("GET", COMPARE, answer(200, files()));
    hub.on("POST", REVIEWS, answer(200, json!({"id": 1})));
    hub.on("POST", THREADS, answer(201, json!({"id": 2})));
    hub
}

fn posted(hub: &mut Hub, l: &Look) -> Result<Posted, Unposted> {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let (_, got) = look(&cfg, hub, &mut States::new("running"), 0.0, |p| {
        Ok(post(p, l))
    });
    got.unwrap()
}

/// What was POSTed, path and body, in order.
fn sent(hub: &Hub) -> Vec<(String, Value)> {
    hub.asked
        .iter()
        .filter(|a| a.0 == "POST")
        .map(|a| (a.1.clone(), a.3.clone().unwrap_or_default()))
        .collect()
}

/// The mark a review carries: what it was of, and how many findings.
fn said(body: &str, reviewer: &str) -> Value {
    let start = format!("<!-- {reviewer}-review:v1 ");
    let at = body.find(&start).unwrap() + start.len();
    let end = body[at..].find(" -->").unwrap();
    serde_json::from_str(&body[at..at + end]).unwrap()
}

#[test]
fn the_lines_a_thread_can_go_on_are_the_new_side_of_githubs_compare() {
    let lines = commentable(&files()).unwrap();
    let of = |name: &str| -> Vec<i64> {
        let (_, l) = lines.iter().find(|(n, _)| n == name).unwrap();
        l.iter().copied().collect()
    };
    assert_eq!(of("src/app.py"), [10, 11, 12, 13]);
    // A name is GitHub's own, never read out of a diff's header.
    assert_eq!(of("cert-é.pem"), [1]);
    assert_eq!(of("config.txt"), [1, 2]);
    assert_eq!(of("my notes.txt"), [1]);
    // Changed, with nothing on the new side to go on.
    for none in ["start.sh", "gone.txt", "logo.png"] {
        assert!(of(none).is_empty(), "{none}");
    }
    assert_eq!(
        lines.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
        [
            "src/app.py",
            "cert-é.pem",
            "config.txt",
            "my notes.txt",
            "start.sh",
            "gone.txt",
            "logo.png"
        ]
    );
    // A hunk whose start can't be read counts nothing until the next one.
    let odd = json!({"files": [{"filename": "a", "patch": "@@ what\n+x\n@@ -1 +5 @@\n+y"}]});
    assert_eq!(
        commentable(&odd).unwrap()[0]
            .1
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        [5]
    );
    // An answer with no list of files, or a file without a name, can't be
    // read: none of it is taken.
    for bad in [
        json!({}),
        json!({"files": {}}),
        json!({"files": [{"status": "added"}]}),
    ] {
        assert!(commentable(&bad).is_none(), "{bad}");
    }
}

#[test]
fn each_finding_goes_where_it_can_and_counts() {
    let mut hub = github();
    let l = of("rival", "Rival", three());
    assert_eq!(posted(&mut hub, &l), Ok(Posted::Findings));
    let posts = sent(&hub);
    assert_eq!(
        posts.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(),
        [REVIEWS, THREADS, THREADS]
    );
    let (review, whole, elsewhere) = (&posts[0].1, &posts[1].1, &posts[2].1);
    assert_eq!(
        (review["commit_id"].as_str(), review["event"].as_str()),
        (Some(HEAD), Some("COMMENT"))
    );
    // Off a changed line, a finding goes on the nearest; the model's text
    // can't ping anyone or forge a mark.
    let line = &review["comments"][0];
    assert_eq!(review["comments"].as_array().unwrap().len(), 1);
    assert_eq!(
        (
            line["path"].as_str(),
            line["line"].as_i64(),
            line["side"].as_str()
        ),
        (Some("src/app.py"), Some(13), Some("RIGHT"))
    );
    let text = line["body"].as_str().unwrap();
    assert!(
        text.starts_with("**Rival · P0** ping @\u{200b}evil\n\nbad &lt;!-- rival-review:v1 {} -->"),
        "{text}"
    );
    // What a complete fix covers goes in the thread, for whoever fixes it.
    assert!(text.contains("\n\n**A complete fix covers:** every caller, @\u{200b}evil's too\n\n<!-- rival-finding -->"), "{text}");
    // A file with no new side gets a thread on the whole file. What its fix
    // covers, blank, says nothing.
    assert_eq!(
        (
            whole["path"].as_str(),
            whole["subject_type"].as_str(),
            whole["commit_id"].as_str()
        ),
        (Some("gone.txt"), Some("file"), Some(HEAD))
    );
    assert_eq!(
        whole["body"],
        "**Rival · P2** t\n\nb\n\n<!-- rival-finding -->"
    );
    // One on a file the PR doesn't change goes on its first changed file,
    // saying where, as a P1 when its severity isn't one.
    assert_eq!(
        (
            elsewhere["path"].as_str(),
            elsewhere["subject_type"].as_str()
        ),
        (Some("src/app.py"), Some("file"))
    );
    assert_eq!(
        elsewhere["body"],
        "**Rival · P1** t2\n\nb2\n\n(In `nope.py`, which this PR doesn't change.)\n\n<!-- rival-finding -->"
    );
    // The review says so, lists the file-level ones in full (a thread
    // GitHub won't take loses nothing), and is marked once, with the
    // description its reviewer was shown.
    let body = review["body"].as_str().unwrap();
    assert!(body.starts_with("**Rival review** of aaaaaaa: 3 findings. Fix or answer each, then resolve its thread.\n\ns\n\n"), "{body}");
    assert!(body.contains("Findings on whole files (each a thread too):\n- `gone.txt`: **Rival · P2** t b\n- `nope.py`: **Rival · P1** t2 b2"), "{body}");
    assert!(
        !body.contains("no changed file to go on") && !body.contains("rival-finding"),
        "{body}"
    );
    assert_eq!(body.matches("<!-- rival-review:v1").count(), 1);
    assert_eq!(
        said(body, "rival"),
        json!({"commit_sha": HEAD, "base": "main", "findings": 3, "description": "4f2a"})
    );
}

#[test]
fn a_thread_github_wont_take_loses_nothing_and_a_review_it_wont_take_leaves_none() {
    let mut hub = github();
    hub.on(
        "POST",
        THREADS,
        answer(422, json!({"message": "Validation Failed"})),
    );
    let l = of("rival", "Rival", three());
    assert_eq!(posted(&mut hub, &l), Ok(Posted::Findings));
    assert_eq!(sent(&hub).len(), 3);
    assert!(
        sent(&hub)[0].1["body"]
            .as_str()
            .unwrap()
            .contains("- `gone.txt`: ")
    );
    // The review turned away: no threads posted for the same answer to
    // leave behind when it goes again next pass.
    let mut hub = github();
    hub.on(
        "POST",
        REVIEWS,
        answer(422, json!({"message": "Validation Failed"})),
    );
    match posted(&mut hub, &l) {
        Err(Unposted::Refused(why)) => assert!(
            why.starts_with("GitHub didn't take the review: POST"),
            "{why}"
        ),
        other => panic!("{other:?}"),
    }
    assert_eq!(sent(&hub).len(), 1);
}

#[test]
fn nothing_found_is_a_clean_review_and_a_finding_with_no_file_to_go_on_is_listed() {
    let mut hub = github();
    let l = of("codex", "Codex", json!({"findings": []}));
    assert_eq!(posted(&mut hub, &l), Ok(Posted::Clean));
    let review = &sent(&hub)[0].1;
    let body = review["body"].as_str().unwrap();
    assert!(
        body.starts_with("**Codex review** of aaaaaaa: nothing found.\n\n<!-- codex-review:v1 "),
        "{body}"
    );
    assert_eq!(said(body, "codex")["findings"], 0);
    assert_eq!(review["comments"], json!([]));
    // A compare with no changed file: the finding's in the review, and
    // still counts.
    let mut hub = github();
    hub.on("GET", COMPARE, answer(200, json!({"files": []})));
    let one = json!({"findings": [three()["findings"][2].clone()]});
    let l = of("rival", "Rival", one);
    assert_eq!(posted(&mut hub, &l), Ok(Posted::Findings));
    let posts = sent(&hub);
    let body = posts[0].1["body"].as_str().unwrap();
    assert_eq!(posts.len(), 1);
    assert!(
        body.contains("1 finding. Fix or answer each")
            && body.contains(
                "Findings with no changed file to go on:\n- `nope.py`: **Rival · P1** t2 b2"
            ),
        "{body}"
    );
    assert_eq!(said(body, "rival")["findings"], 1);
}

#[test]
fn a_line_is_read_as_a_number_where_it_can_be() {
    for (line, at) in [
        (json!(12), 12),
        (json!("12"), 12),
        (json!(" 11 "), 11),
        (json!(12.9), 12),
        (json!(true), 10),
        (json!([12]), 10),
        (json!(-4), 10),
        (json!("twelve"), 10),
        (json!(null), 10),
        (json!(1e300), 13),
        (json!(18446744073709551615u64), 13),
    ] {
        let mut hub = github();
        let f =
            json!({"findings": [{"path": "src/app.py", "line": line, "title": "t", "body": "b"}]});
        assert_eq!(
            posted(&mut hub, &of("rival", "Rival", f)),
            Ok(Posted::Findings)
        );
        assert_eq!(sent(&hub)[0].1["comments"][0]["line"], at, "{line}");
    }
    // No line at all: the file's first that can take one.
    let mut hub = github();
    let f = json!({"findings": [{"path": "src/app.py", "title": "t", "body": "b"}]});
    assert_eq!(
        posted(&mut hub, &of("rival", "Rival", f)),
        Ok(Posted::Findings)
    );
    assert_eq!(sent(&hub)[0].1["comments"][0]["line"], 10);
}

#[test]
fn an_answer_without_a_list_of_findings_is_no_review() {
    for bad in [
        json!({"summary": "couldn't finish"}),
        json!({"findings": null}),
        json!({"findings": ["x"]}),
        json!(["not", "a", "dict"]),
        json!({"findings": [{"path": ["src/app.py"], "line": 1}]}),
        json!({"findings": [{"path": "src/app.py", "title": {}}]}),
        json!({"findings": [], "summary": 3}),
        json!({"findings": [], "summary": null}),
        json!({"findings": [{"line": 1, "severity": "P1", "title": "t", "body": "b"}]}),
    ] {
        let mut hub = github();
        match posted(&mut hub, &of("rival", "Rival", bad.clone())) {
            Err(Unposted::NotAReview(why)) => assert!(why.starts_with("not a review: "), "{why}"),
            other => panic!("{bad} gave {other:?}"),
        }
        assert!(hub.asked.is_empty(), "{bad}");
    }
}

#[test]
fn a_compare_that_cant_be_read_or_github_down_posts_nothing() {
    // Read as one, every finding would land in the body and the review
    // count as clean.
    for got in [json!({}), json!({"files": "x"}), json!([])] {
        let mut hub = github();
        hub.on("GET", COMPARE, answer(200, got.clone()));
        match posted(&mut hub, &of("rival", "Rival", three())) {
            Err(Unposted::Refused(why)) => assert!(
                why.starts_with("couldn't read the diff: GET repos/sam/r/compare/"),
                "{why}"
            ),
            other => panic!("{got} gave {other:?}"),
        }
        assert!(sent(&hub).is_empty(), "{got}");
    }
    // GitHub not answering is told apart from GitHub saying no, for the
    // compare and for the review.
    let mut hub = github();
    hub.on(
        "GET",
        COMPARE,
        answer(502, json!({"message": "Server Error"})),
    );
    assert!(matches!(
        posted(&mut hub, &of("rival", "Rival", three())),
        Err(Unposted::Unanswered(_))
    ));
    assert!(sent(&hub).is_empty());
    let mut hub = github();
    hub.on("GET", COMPARE, answer(404, json!({"message": "Not Found"})));
    assert!(matches!(
        posted(&mut hub, &of("rival", "Rival", three())),
        Err(Unposted::Refused(_))
    ));
    let mut hub = github();
    hub.on(
        "POST",
        REVIEWS,
        answer(502, json!({"message": "Server Error"})),
    );
    assert!(matches!(
        posted(&mut hub, &of("rival", "Rival", three())),
        Err(Unposted::Unanswered(_))
    ));
    // Sent once: GitHub may have made it.
    assert_eq!(sent(&hub).len(), 1);
}

#[test]
fn a_base_and_head_go_in_the_compare_whole() {
    let mut hub = Hub::default();
    hub.on(
        "GET",
        "repos/sam/r/compare/release%2F1.0%23x...aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa$",
        answer(200, files()),
    );
    hub.on("POST", REVIEWS, answer(200, json!({"id": 1})));
    let mut l = of("codex", "Codex", json!({"findings": []}));
    l.base = "release/1.0#x".into();
    l.description = None;
    assert_eq!(posted(&mut hub, &l), Ok(Posted::Clean));
    let body = sent(&hub)[0].1["body"].as_str().unwrap().to_string();
    assert_eq!(
        said(&body, "codex"),
        json!({"commit_sha": HEAD, "base": "release/1.0#x", "findings": 0, "description": null})
    );
}

#[test]
fn quiet_text_pings_nobody_and_marks_nothing() {
    assert_eq!(
        quiet("@evil @-x @é a@b @ <!-- x -->"),
        "@\u{200b}evil @\u{200b}-x @\u{200b}é a@\u{200b}b @ &lt;!-- x -->"
    );
    assert_eq!(quiet("@"), "@");
}
