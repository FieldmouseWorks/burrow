use super::*;
use burrow_github::fake::{Fake, ok};
use serde_json::json;
use std::fs;

fn github(author: &str, body: &str, editor: Option<&str>) -> Fake {
    let mut gh = Fake::default();
    let edits = editor
        .map(|e| vec![json!({"editor":burrow_github::fake::actor(e),"deletedBy":null})])
        .unwrap_or_default();
    gh.on(
        "GET",
        "repos/sam/box/issues",
        vec![ok(200, json!([{"number":1,"user":burrow_github::fake::actor(author)}]), &[])],
    )
    .on(
        "POST",
        "graphql",
        vec![ok(
            200,
            json!({"data":{"repository":{"issue":{
                "id":"issue-one", "author":burrow_github::fake::actor(author),"title":"engines","body":body,
                "openCycle":{"nodes":[]},
                "userContentEdits":{"totalCount":edits.len(),"nodes":edits},
                "timelineItems":{"totalCount":0,"nodes":[]}
            }}}}),
            &[],
        )],
    )
    .on(
        "POST",
        "repos/sam/box/issues/1/comments",
        vec![ok(201, json!({}), &[])],
    )
    .on(
        "PATCH",
        "repos/sam/box/issues/1",
        vec![ok(200, json!({}), &[])],
    );
    gh
}

#[test]
fn switch_bad_setup_and_non_authors() {
    let d = tempfile::tempdir().unwrap();
    let roles = d.path().join("roles");
    fs::create_dir(d.path().join("roles.d")).unwrap();
    fs::write(d.path().join("roles.d/first"), "work claude\n").unwrap();
    fs::write(d.path().join("roles.d/next"), "work@sam/code codex\n").unwrap();
    fs::write(
        d.path().join("roles.d/bad"),
        "# comment\njudge old-format\n",
    )
    .unwrap();
    fs::write(&roles, "work claude\n").unwrap();
    let cfg = Config {
        repo: "sam/box".into(),
        label: "engines".into(),
        authors: vec![burrow_github::fake::author("sam")],
        roles,
        state: d.path().join("state"),
    };
    let mut gh = github("sam", "use next", None);
    pass(&mut gh, &cfg).unwrap();
    let link = fs::read_link(&cfg.roles).unwrap();
    let comment = gh
        .asked("POST")
        .into_iter()
        .find(|a| a.1.ends_with("/comments"))
        .unwrap()
        .2
        .as_ref()
        .unwrap()["body"]
        .as_str()
        .unwrap();
    assert!(
        comment.contains("Was active: first\nActive: next")
            && comment.contains("work@sam/code codex")
    );
    assert_eq!(gh.asked("PATCH").len(), 1);
    for body in [
        "use bad",
        "use absent",
        "use ../first",
        "use first\nuse next",
    ] {
        let mut gh = github("sam", body, None);
        cycle(&mut gh, body);
        pass(&mut gh, &cfg).unwrap();
        assert_eq!(fs::read_link(&cfg.roles).unwrap(), link);
        let text = gh
            .asked("POST")
            .last()
            .unwrap()
            .2
            .as_ref()
            .unwrap()
            .to_string();
        assert!(!text.contains("old-format"));
        if body == "use bad" {
            assert!(text.contains("line 2"), "{text}");
        }
    }
    for (author, editor) in [("outsider", None), ("sam", Some("outsider"))] {
        let mut gh = github(author, "use first", editor);
        pass(&mut gh, &cfg).unwrap();
        assert_eq!(fs::read_link(&cfg.roles).unwrap(), link);
        assert!(gh.asked("PATCH").is_empty());
    }
}

#[test]
fn a_failed_reply_does_not_repeat_back() {
    let d = tempfile::tempdir().unwrap();
    let roles = d.path().join("roles");
    fs::create_dir(d.path().join("roles.d")).unwrap();
    fs::write(&roles, "work claude\n").unwrap();
    fs::write(d.path().join("roles.d/first"), "work claude\n").unwrap();
    fs::write(d.path().join("roles.d/second"), "work codex\n").unwrap();
    let store = Store::new(&roles);
    store.command(&["use".into(), "second".into()]).unwrap();
    let cfg = Config {
        repo: "sam/box".into(),
        label: "engines".into(),
        authors: vec![burrow_github::fake::author("sam")],
        roles,
        state: d.path().join("state"),
    };
    let mut gh = github("sam", "use back", None);
    // Comment succeeds, close fails. The next pass must only repeat the reply.
    gh.answers
        .iter_mut()
        .find(|(method, _, _)| method == "PATCH")
        .unwrap()
        .2 = vec![ok(403, json!({"message":"refused"}), &[])].into();
    assert!(pass(&mut gh, &cfg).is_err());
    assert_eq!(fs::read_to_string(&cfg.roles).unwrap(), "work claude\n");
    let mut gh = github("sam", "use back", None);
    pass(&mut gh, &cfg).unwrap();
    assert_eq!(fs::read_to_string(&cfg.roles).unwrap(), "work claude\n");
    // Once closed successfully a reopened issue really does ask again.
    cycle(&mut gh, "second-open");
    pass(&mut gh, &cfg).unwrap();
    assert_eq!(fs::read_to_string(&cfg.roles).unwrap(), "work codex\n");
}

fn cycle(gh: &mut Fake, id: &str) {
    for (method, path, answers) in &mut gh.answers {
        if method == "POST" && path == "graphql" {
            for answer in answers {
                let response = answer.as_mut().unwrap();
                let mut json: serde_json::Value = serde_json::from_str(&response.body).unwrap();
                json["data"]["repository"]["issue"]["openCycle"] = json!({"nodes":[{"id":id}]});
                response.body = json.to_string();
            }
        }
    }
}

#[test]
fn ambiguous_close_and_reopen_is_a_new_request_even_with_an_old_receipt() {
    let d = tempfile::tempdir().unwrap();
    let roles = d.path().join("roles");
    fs::create_dir(d.path().join("roles.d")).unwrap();
    fs::write(&roles, "work claude\n").unwrap();
    fs::write(d.path().join("roles.d/one"), "work claude\n").unwrap();
    fs::write(d.path().join("roles.d/two"), "work codex\n").unwrap();
    Store::new(&roles)
        .command(&["use".into(), "two".into()])
        .unwrap();
    let cfg = Config {
        repo: "sam/box".into(),
        label: "engines".into(),
        authors: vec![burrow_github::fake::author("sam")],
        roles,
        state: d.path().join("state"),
    };
    let mut gh = github("sam", "use back", None);
    gh.answers
        .iter_mut()
        .find(|(m, _, _)| m == "PATCH")
        .unwrap()
        .2 = vec![Err(burrow_github::Failed {
        why: "close accepted but response lost".into(),
        transient: false,
    })]
    .into();
    assert!(pass(&mut gh, &cfg).is_err());
    assert_eq!(fs::read_to_string(&cfg.roles).unwrap(), "work claude\n");
    // GitHub may have closed it despite the lost response, or a person may
    // close it manually. The latest reopen event distinguishes both cases.
    let mut reopened = github("sam", "use back", None);
    cycle(&mut reopened, "reopened-after-ambiguous-close");
    pass(&mut reopened, &cfg).unwrap();
    assert_eq!(fs::read_to_string(&cfg.roles).unwrap(), "work codex\n");
    pass(&mut reopened, &cfg).unwrap(); // same open cycle: only a reply retry
    assert_eq!(fs::read_to_string(&cfg.roles).unwrap(), "work codex\n");
}

#[test]
fn reply_receipt_write_failure_and_an_intervening_cli_do_not_repeat_back() {
    let d = tempfile::tempdir().unwrap();
    let roles = d.path().join("roles");
    fs::create_dir(d.path().join("roles.d")).unwrap();
    for (name, text) in [
        ("one", "work claude\n"),
        ("two", "work codex\n"),
        ("three", "work codex over 80 claude\n"),
    ] {
        fs::write(d.path().join("roles.d").join(name), text).unwrap();
    }
    let store = Store::new(&roles);
    store.command(&["use".into(), "two".into()]).unwrap();
    store.command(&["use".into(), "one".into()]).unwrap();
    store.command(&["use".into(), "two".into()]).unwrap();
    let cfg = Config {
        repo: "sam/box".into(),
        label: "engines".into(),
        authors: vec![burrow_github::fake::author("sam")],
        roles,
        state: d.path().join("state"),
    };
    let state = cfg.state.join("sam--box");
    fs::create_dir_all(&state).unwrap();
    let hash: String = Sha256::digest(b"issue-one:initial")
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let blocked = state.join(format!("1-{hash}.json"));
    // The switch ran and its operation was recorded, but the door's receipt
    // can't be stored: the name is taken by a directory.
    Store::new(&cfg.roles)
        .request(
            &blocked.with_extension("operation"),
            &["use".into(), "back".into()],
        )
        .unwrap();
    fs::create_dir(&blocked).unwrap();
    let mut gh = github("sam", "use back", None);
    assert!(pass(&mut gh, &cfg).is_err());
    assert_eq!(fs::read_to_string(&cfg.roles).unwrap(), "work claude\n");
    Store::new(&cfg.roles)
        .command(&["use".into(), "three".into()])
        .unwrap();
    fs::remove_dir(blocked).unwrap();
    pass(&mut gh, &cfg).unwrap();
    assert_eq!(
        fs::read_to_string(&cfg.roles).unwrap(),
        "work codex over 80 claude\n"
    );
}

#[test]
fn rate_limits_defer_at_every_door_stage() {
    for stage in [
        "list",
        "author",
        "comment",
        "close",
        "graphql-primary",
        "graphql-secondary",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let wake = dir.path().join("wake.json");
        let cfg = Config {
            repo: "sam/box".into(),
            label: "engines".into(),
            authors: vec![burrow_github::fake::author("sam")],
            roles: dir.path().join("roles"),
            state: dir.path().join("receipts"),
        };
        let mut gh = github("sam", "invalid request", None);
        let now = burrow_github::Api::now(&gh);
        let reset = (now + 600).to_string();
        let (status, body) = match stage {
            "graphql-primary" => (
                200,
                json!({"errors":[{"type":"RATE_LIMITED","message":"budget exhausted"}]}),
            ),
            "graphql-secondary" => (
                200,
                json!({"errors":[{"message":"You have exceeded a secondary rate limit"}]}),
            ),
            _ => (403, json!({"message":"API rate limit exceeded"})),
        };
        for (method, path, replies) in &mut gh.answers {
            let matches = match stage {
                "list" => method == "GET" && path.ends_with("/issues"),
                "author" | "graphql-primary" | "graphql-secondary" => path == "graphql",
                "comment" => method == "POST" && path.contains("/issues/"),
                "close" => method == "PATCH",
                _ => false,
            };
            if matches {
                *replies = vec![ok(
                    status,
                    body.clone(),
                    &[
                        ("x-ratelimit-remaining", "0"),
                        ("x-ratelimit-reset", &reset),
                    ],
                )]
                .into();
            }
        }
        let result = burrow_github::deferred::run(Some(&wake), &mut gh, now, |api| pass(api, &cfg));
        assert!(matches!(result, Ok(None)), "{stage}: {result:?}");
        let booked: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&wake).unwrap()).unwrap();
        assert_eq!(booked[""], now + 600, "{stage}");
        if stage != "close" {
            assert!(gh.asked("PATCH").is_empty(), "{stage}: request closed");
        }
    }
}
