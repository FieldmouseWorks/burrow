use burrow_github::fake::{Fake, ok};
use serde_json::{Value, json};

use super::{Done, MAX_QUESTIONS, pass, questions};
use crate::query::Query;
use crate::{Config, Journal, Tools};

fn config() -> Config {
    Config {
        repo: "sam/box".into(),
        label: "probe".into(),
        authors: vec!["Sam".into()],
        host: "box".into(),
        user: "dev".into(),
        journal: Journal {
            units: vec![],
            user_units: vec![],
        },
        state_dir: "/var/lib/burrow".into(),
        proj: None,
        tools: Tools {
            journalctl: "/bin/journalctl".into(),
            systemctl: "/bin/systemctl".into(),
        },
    }
}

fn issue(n: u64, login: &str, title: &str, body: Value) -> Value {
    json!({ "number": n, "user": { "login": login }, "title": title, "body": body })
}

fn github(private: bool, issues: Value) -> Fake {
    let mut gh = Fake::default();
    gh.on("GET", "repos/sam/box/issues", vec![ok(200, issues, &[])])
        .on(
            "GET",
            "repos/sam/box",
            vec![ok(200, json!({ "private": private }), &[])],
        )
        .on(
            "POST",
            "repos/sam/box/issues/",
            vec![ok(201, json!({}), &[])],
        )
        .on(
            "PATCH",
            "repos/sam/box/issues/",
            vec![ok(200, json!({}), &[])],
        );
    gh
}

/// Each question's answer: its own words, and a secret beside them.
fn answer(q: &Query) -> Result<String, String> {
    Ok(format!("{q} says hi with gh{}_{}\n", "p", "Zz9".repeat(12)))
}

fn bodies(gh: &Fake) -> Vec<String> {
    gh.asked("POST")
        .iter()
        .map(|a| a.2.as_ref().unwrap()["body"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn an_authors_issue_is_answered_scrubbed_then_closed() {
    let mut gh = github(
        true,
        json!([issue(
            7,
            "sam",
            "probe",
            json!("memory\n\n`journal box-update -1h now`\nfrobnicate\n")
        )]),
    );
    let done = pass(&mut gh, &config(), &mut answer, "2026-11-01 14:00:00Z").unwrap();
    assert_eq!(
        done,
        Done {
            answered: vec![7],
            refused: vec![]
        }
    );
    let posts = gh.asked("POST");
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].1, "repos/sam/box/issues/7/comments");
    let body = &bodies(&gh)[0];
    assert!(
        body.starts_with("<!-- burrow-probe -->\nThe probe on box, at 2026-11-01 14:00:00Z:\n"),
        "{body}"
    );
    assert!(
        body.contains("**`memory`**\n\n```text\nmemory says hi with [redacted]\n```"),
        "{body}"
    );
    assert!(
        body.contains("**`journal box-update.service -1h now`**"),
        "{body}"
    );
    assert!(
        body.contains("**`frobnicate`**: \"frobnicate\" isn't a question"),
        "{body}"
    );
    assert!(!body.contains(&format!("gh{}_", "p")), "{body}");
    // The close comes after the comment, as done.
    let patch = gh.asked("PATCH");
    assert_eq!(patch[0].1, "repos/sam/box/issues/7");
    assert_eq!(
        patch[0].2.as_ref().unwrap(),
        &json!({ "state": "closed", "state_reason": "completed" })
    );
    let order: Vec<&str> = gh.asked.iter().map(|a| a.0.as_str()).collect();
    assert_eq!(order, ["GET", "GET", "POST", "PATCH"]);
}

#[test]
fn anyone_elses_issue_runs_nothing() {
    let mut gh = github(
        true,
        json!([issue(
            8,
            "mallory",
            "probe",
            json!("journal box-update -1h now")
        )]),
    );
    let mut ran = 0;
    let done = pass(
        &mut gh,
        &config(),
        &mut |_| {
            ran += 1;
            Ok(String::new())
        },
        "now",
    )
    .unwrap();
    assert_eq!(ran, 0);
    assert_eq!(
        done,
        Done {
            answered: vec![],
            refused: vec![8]
        }
    );
    assert_eq!(
        bodies(&gh),
        ["<!-- burrow-probe -->\nOnly Sam can ask this machine's probe, so nothing was run."]
    );
    assert_eq!(
        gh.asked("PATCH")[0].2.as_ref().unwrap()["state_reason"],
        "not_planned"
    );
}

#[test]
fn a_repo_that_isnt_private_gets_nothing() {
    let mut gh = github(false, json!([issue(9, "sam", "memory", json!(null))]));
    let mut ran = 0;
    let e = pass(
        &mut gh,
        &config(),
        &mut |_| {
            ran += 1;
            Ok(String::new())
        },
        "now",
    )
    .unwrap_err();
    assert!(e.contains("isn't private"), "{e}");
    assert_eq!(ran, 0);
    assert!(gh.asked("POST").is_empty() && gh.asked("PATCH").is_empty());
    // Nor one GitHub says nothing about.
    let mut gh = Fake::default();
    gh.on("GET", "repos/sam/box", vec![ok(200, json!({}), &[])]);
    assert!(pass(&mut gh, &config(), &mut answer, "now").is_err());
}

#[test]
fn a_comment_that_fails_leaves_the_issue_open() {
    let mut gh = Fake::default();
    gh.on(
        "GET",
        "repos/sam/box/issues",
        vec![ok(
            200,
            json!([
                issue(3, "sam", "rc", json!(null)),
                issue(4, "sam", "queue", json!(null))
            ]),
            &[],
        )],
    )
    .on(
        "GET",
        "repos/sam/box",
        vec![ok(200, json!({ "private": true }), &[])],
    )
    .on(
        "POST",
        "repos/sam/box/issues/3/",
        vec![ok(422, json!({ "message": "no" }), &[])],
    )
    .on(
        "POST",
        "repos/sam/box/issues/4/",
        vec![ok(201, json!({}), &[])],
    )
    .on(
        "PATCH",
        "repos/sam/box/issues/",
        vec![ok(200, json!({}), &[])],
    );
    let e = pass(&mut gh, &config(), &mut answer, "now").unwrap_err();
    assert!(e.contains("#3"), "{e}");
    let closed: Vec<&str> = gh.asked("PATCH").iter().map(|a| a.1.as_str()).collect();
    assert_eq!(closed, ["repos/sam/box/issues/4"]);
}

#[test]
fn pull_requests_and_every_page() {
    let mut gh = Fake::default();
    gh.on(
        "GET",
        "repos/sam/box/issues?",
        vec![ok(
            200,
            json!([{ "number": 1, "pull_request": {}, "user": { "login": "sam" } }]),
            &[(
                "Link",
                "<https://api.github.com/repos/sam/box/issues/page2>; rel=\"next\"",
            )],
        )],
    )
    .on(
        "GET",
        "repos/sam/box/issues/page2",
        vec![ok(200, json!([issue(2, "sam", "units", json!(""))]), &[])],
    )
    .on(
        "GET",
        "repos/sam/box",
        vec![ok(200, json!({ "private": true }), &[])],
    )
    .on(
        "POST",
        "repos/sam/box/issues/",
        vec![ok(201, json!({}), &[])],
    )
    .on(
        "PATCH",
        "repos/sam/box/issues/",
        vec![ok(200, json!({}), &[])],
    );
    let done = pass(&mut gh, &config(), &mut answer, "now").unwrap();
    assert_eq!(done.answered, [2]);
    let listed = &gh.asked("GET")[1].1;
    assert_eq!(
        listed,
        "repos/sam/box/issues?state=open&labels=probe&sort=created&direction=asc&per_page=100"
    );
}

#[test]
fn what_an_issue_asks() {
    let q = |title: &str, body: Value| questions(&issue(1, "sam", title, body));
    assert_eq!(q("memory", json!(null)), ["memory"]);
    assert_eq!(
        q(
            "ignored",
            json!("```\nrc\n```\n<!-- a note -->\n- `queue`\n\n  units  ")
        ),
        ["rc", "queue", "units"]
    );
    assert!(q("", json!("")).is_empty());
    // Past the limit, the rest are named, not run.
    let many = ["memory"; MAX_QUESTIONS + 2].join("\n");
    let mut gh = github(true, json!([issue(5, "sam", "x", json!(many))]));
    let mut ran = 0;
    pass(
        &mut gh,
        &config(),
        &mut |_| {
            ran += 1;
            Ok(String::new())
        },
        "now",
    )
    .unwrap();
    assert_eq!(ran, MAX_QUESTIONS);
    assert!(bodies(&gh)[0].contains("ask the other 2 in another issue"));
}

#[test]
fn an_answer_too_big_for_a_comment_keeps_its_newest_lines() {
    let mut gh = github(
        true,
        json!([issue(6, "sam", "x", json!("journal box-update -1d now"))]),
    );
    let big = (0..20_000)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    pass(&mut gh, &config(), &mut |_| Ok(big.clone()), "now").unwrap();
    let body = &bodies(&gh)[0];
    assert!(body.len() < 65_536, "{}", body.len());
    assert!(
        body.contains("earlier lines cut") && body.contains("line 19999\n```"),
        "{}",
        &body[..300]
    );
    // And backticks in an answer can't end its block.
    let mut gh = github(true, json!([issue(6, "sam", "x", json!("memory"))]));
    pass(&mut gh, &config(), &mut |_| Ok("a ```` b".into()), "now").unwrap();
    assert!(bodies(&gh)[0].contains("`````text\na ```` b\n`````"));
}
