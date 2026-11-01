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

/// GitHub's word on who wrote an issue: `opener`, then each edit's editor
/// and each title change's actor (null for one GitHub doesn't name).
fn writers(opener: &str, title: &str, body: Value, editors: &[Value], renamers: &[Value]) -> Value {
    let edits: Vec<Value> = editors
        .iter()
        .map(|e| json!({ "editor": if e.is_null() { Value::Null } else { json!({ "login": e }) }, "deletedBy": null }))
        .collect();
    let renames: Vec<Value> = renamers
        .iter()
        .map(|a| json!({ "actor": if a.is_null() { Value::Null } else { json!({ "login": a }) } }))
        .collect();
    json!({ "data": { "repository": { "issue": {
        "author": { "login": opener },
        "title": title,
        "body": body,
        "userContentEdits": { "totalCount": edits.len(), "nodes": edits },
        "timelineItems": { "totalCount": renames.len(), "nodes": renames },
    } } } })
}

/// GitHub's word on each of `issues` an author opened, in order: nobody
/// else wrote any of it.
fn unedited(gh: &mut Fake, issues: &Value) {
    let said: Vec<_> = issues
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| {
            i.get("pull_request").is_none()
                && i["user"]["login"]
                    .as_str()
                    .unwrap()
                    .eq_ignore_ascii_case("sam")
        })
        .map(|i| {
            ok(
                200,
                writers(
                    "sam",
                    i["title"].as_str().unwrap(),
                    i["body"].clone(),
                    &[],
                    &[],
                ),
                &[],
            )
        })
        .collect();
    if !said.is_empty() {
        gh.on("POST", "graphql", said);
    }
}

fn github(private: bool, issues: Value) -> Fake {
    let mut gh = Fake::default();
    unedited(&mut gh, &issues);
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

/// The comments posted.
fn comments(gh: &Fake) -> Vec<&burrow_github::fake::Asked> {
    gh.asked("POST")
        .into_iter()
        .filter(|a| a.1.starts_with("repos/"))
        .collect()
}

fn bodies(gh: &Fake) -> Vec<String> {
    comments(gh)
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
    let posts = comments(&gh);
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
    let order: Vec<&str> = gh.asked.iter().map(|a| a.1.as_str()).collect();
    assert_eq!(
        order,
        [
            "repos/sam/box",
            "repos/sam/box/issues?state=open&labels=probe&sort=created&direction=asc&per_page=100",
            "graphql",
            "repos/sam/box/issues/7/comments",
            "repos/sam/box/issues/7"
        ]
    );
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
    assert!(gh.asked.iter().all(|a| a.1 != "graphql"));
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
    let issues = json!([
        issue(3, "sam", "rc", json!(null)),
        issue(4, "sam", "queue", json!(null))
    ]);
    unedited(&mut gh, &issues);
    gh.on("GET", "repos/sam/box/issues", vec![ok(200, issues, &[])])
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
        "POST",
        "graphql",
        vec![ok(200, writers("sam", "units", json!(""), &[], &[]), &[])],
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

/// An author's issue that someone else changed, or that GitHub can't say
/// who changed, runs nothing; one only authors changed runs what it says
/// now, whatever the list said.
#[test]
fn what_runs_is_text_only_authors_wrote() {
    let refused = |said: Value| {
        let mut gh = Fake::default();
        gh.on(
            "GET",
            "repos/sam/box/issues",
            vec![ok(
                200,
                json!([issue(4, "sam", "probe", json!("memory"))]),
                &[],
            )],
        )
        .on(
            "GET",
            "repos/sam/box",
            vec![ok(200, json!({ "private": true }), &[])],
        )
        .on("POST", "graphql", vec![ok(200, said, &[])])
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
        (
            ran,
            done,
            bodies(&gh),
            gh.asked("PATCH")[0].2.clone().unwrap(),
        )
    };
    let body = json!("journal box-update -1h now");
    let mut too_many = writers("sam", "probe", body.clone(), &[json!("sam")], &[]);
    too_many["data"]["repository"]["issue"]["userContentEdits"]["totalCount"] = json!(101);
    let mut deleted = writers("sam", "probe", body.clone(), &[json!("sam")], &[]);
    deleted["data"]["repository"]["issue"]["userContentEdits"]["nodes"][0]["deletedBy"] =
        json!({ "login": "mallory" });
    for said in [
        // Its body, edited by someone else, before a first pass or after a reopen.
        writers(
            "sam",
            "probe",
            body.clone(),
            &[json!("sam"), json!("mallory")],
            &[],
        ),
        // By someone GitHub doesn't name.
        writers("sam", "probe", body.clone(), &[Value::Null], &[]),
        // Its title, renamed by someone else (the title asks when the body is empty).
        writers("sam", "memory", json!(""), &[], &[json!("mallory")]),
        writers("sam", "memory", json!(""), &[], &[Value::Null]),
        // More edits than one read holds, and one whose record someone else deleted.
        too_many,
        deleted,
        // Opened, GitHub says now, by someone else.
        writers("mallory", "probe", body.clone(), &[], &[]),
    ] {
        let (ran, done, bodies, closed) = refused(said.clone());
        assert_eq!(ran, 0, "{said}");
        assert_eq!(done.refused, [4], "{said}");
        assert!(
            bodies[0].contains("written or changed by someone other than Sam"),
            "{}",
            bodies[0]
        );
        assert_eq!(closed["state_reason"], "not_planned");
    }
    // Only authors changed it: what runs is what GitHub says now, not the list.
    let mut asked = Vec::new();
    let mut gh = Fake::default();
    gh.on(
        "GET",
        "repos/sam/box/issues",
        vec![ok(
            200,
            json!([issue(4, "sam", "probe", json!("memory"))]),
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
        "graphql",
        vec![ok(
            200,
            writers(
                "sam",
                "probe",
                json!("units"),
                &[json!("Sam"), json!("sam")],
                &[json!("sam")],
            ),
            &[],
        )],
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
    let done = pass(
        &mut gh,
        &config(),
        &mut |q| {
            asked.push(q.to_string());
            Ok(String::new())
        },
        "now",
    )
    .unwrap();
    assert_eq!(done.answered, [4]);
    assert_eq!(asked, ["units"]);
    let graphql = &gh.asked("POST")[0];
    assert_eq!(graphql.1, "graphql");
    assert_eq!(
        graphql.2.as_ref().unwrap()["variables"],
        json!({ "owner": "sam", "name": "box", "n": 4 })
    );
}

/// A key cut by a journal's line limit, its END kept and its BEGIN not, or
/// by a comment's share, goes whole either way.
#[test]
fn a_key_cut_short_is_hidden_all_the_same() {
    let payload = "MIIEowIBAAKCAQEAwhatever0123456789";
    let end = format!("-----END RSA {} KEY-----", "PRIVATE");
    // The journal's tail: the payload and END, no BEGIN.
    let tail = format!("unit, a span: its last 400 lines\n{payload}\n{payload}\n{end}\nafter\n");
    let mut gh = github(
        true,
        json!([issue(6, "sam", "x", json!("journal box-update -1d now"))]),
    );
    pass(&mut gh, &config(), &mut |_| Ok(tail.clone()), "now").unwrap();
    let body = &bodies(&gh)[0];
    assert!(
        !body.contains(payload) && body.contains("[redacted private key]\nafter"),
        "{body}"
    );
    // A whole key in an answer too big for its share: the cut keeps the
    // newest lines, the END and the payload's end, and none of it shows.
    let begin = format!("-----BEGIN RSA {} KEY-----", "PRIVATE");
    let mut big: Vec<String> = vec![begin];
    big.extend((0..20_000).map(|i| format!("{payload}{i}")));
    big.push(end.clone());
    big.push("after".into());
    let big = big.join("\n");
    let mut gh = github(
        true,
        json!([issue(6, "sam", "x", json!("journal box-update -1d now"))]),
    );
    pass(&mut gh, &config(), &mut |_| Ok(big.clone()), "now").unwrap();
    let body = &bodies(&gh)[0];
    assert!(
        !body.contains(payload) && !body.contains(&end),
        "{}",
        &body[..300]
    );
    // Hidden as the answer it is, before the cut: the rest of the comment
    // stays, where a key cut first and hidden after takes all before it.
    assert!(
        body.starts_with("<!-- burrow-probe -->\nThe probe on box")
            && body.contains("[redacted private key]\nafter\n```"),
        "{}",
        &body[..300]
    );
}

/// However long what an issue asks, the comment fits in one: an echoed
/// question and why it isn't one are clipped, and whatever's left is cut.
#[test]
fn a_comment_always_fits() {
    let long = "x".repeat(40_000);
    let mut gh = github(true, json!([issue(6, "sam", "x", json!(long))]));
    pass(&mut gh, &config(), &mut answer, "now").unwrap();
    let body = &bodies(&gh)[0];
    assert!(body.len() < 2_000, "{}", body.len());
    assert!(body.contains("more characters)"), "{body}");
    // Ten answers whose fences grow with their backticks: each fits its
    // share, so only the comment's own cut holds them.
    let ticks = "`".repeat(4_500);
    let many = ["memory"; MAX_QUESTIONS].join("\n");
    let mut gh = github(true, json!([issue(6, "sam", "x", json!(many))]));
    pass(&mut gh, &config(), &mut |_| Ok(ticks.clone()), "now").unwrap();
    let body = &bodies(&gh)[0];
    assert!(body.len() <= 60_000, "{}", body.len());
    assert!(body.ends_with("(cut here, past what a comment holds)"));
}
