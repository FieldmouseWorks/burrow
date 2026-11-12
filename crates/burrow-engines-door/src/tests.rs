use super::*;
use burrow_github::fake::{Fake, ok};
use serde_json::json;
use std::fs;
use std::path::Path;

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

fn config(dir: &Path) -> Config {
    Config {
        repo: "sam/box".into(),
        label: "engines".into(),
        authors: vec![burrow_github::fake::author("sam")],
        settings: burrow_settings::Config {
            overlay: dir.join("overlay.json"),
            ..Default::default()
        },
        state: dir.join("state"),
    }
}

/// Save one set per `(name, ROLE ENGINE...)`, leaving the last in force.
fn sets(cfg: &Config, saved: &[(&str, &str)]) {
    let say = |line: &str| {
        let args: Vec<String> = line.split_whitespace().map(String::from).collect();
        cfg.settings.command(&args, "test").unwrap()
    };
    for (name, role) in saved {
        say(&format!("role {role}"));
        say(&format!("engines save {name}"));
    }
}

fn engines(cfg: &Config) -> String {
    cfg.settings.engines().unwrap().roles_text()
}

fn use_set(cfg: &Config, name: &str) {
    cfg.settings
        .command(&["engines".into(), "use".into(), name.into()], "test")
        .unwrap();
}

#[test]
fn switch_bad_setup_and_non_authors() {
    let d = tempfile::tempdir().unwrap();
    let cfg = config(d.path());
    sets(
        &cfg,
        &[("next", "work@sam/code codex"), ("first", "work claude")],
    );
    let mut gh = github("sam", "use next", None);
    pass(&mut gh, &cfg).unwrap();
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
            && comment.contains("work@sam/code codex"),
        "{comment}"
    );
    assert_eq!(gh.asked("PATCH").len(), 1);
    let before = engines(&cfg);
    for body in ["use absent", "use ../first", "use first\nuse next"] {
        let mut gh = github("sam", body, None);
        cycle(&mut gh, body);
        pass(&mut gh, &cfg).unwrap();
        assert_eq!(engines(&cfg), before);
        let text = gh
            .asked("POST")
            .last()
            .unwrap()
            .2
            .as_ref()
            .unwrap()
            .to_string();
        assert!(!text.contains("../first"));
    }
    for (author, editor) in [("outsider", None), ("sam", Some("outsider"))] {
        let mut gh = github(author, "use first", editor);
        pass(&mut gh, &cfg).unwrap();
        assert_eq!(engines(&cfg), before);
        assert!(gh.asked("PATCH").is_empty());
    }
}

#[test]
fn a_failed_reply_does_not_repeat_back() {
    let d = tempfile::tempdir().unwrap();
    let cfg = config(d.path());
    sets(&cfg, &[("first", "work claude"), ("second", "work codex")]);
    let mut gh = github("sam", "use back", None);
    // Comment succeeds, close fails. The next pass must only repeat the reply.
    gh.answers
        .iter_mut()
        .find(|(method, _, _)| method == "PATCH")
        .unwrap()
        .2 = vec![ok(403, json!({"message":"refused"}), &[])].into();
    // Nothing earlier switched, so back has nowhere to go; go there first.
    use_set(&cfg, "first");
    use_set(&cfg, "second");
    assert!(pass(&mut gh, &cfg).is_err());
    assert_eq!(engines(&cfg), "work claude\n");
    let mut gh = github("sam", "use back", None);
    pass(&mut gh, &cfg).unwrap();
    assert_eq!(engines(&cfg), "work claude\n");
    // Once closed successfully a reopened issue really does ask again.
    cycle(&mut gh, "second-open");
    pass(&mut gh, &cfg).unwrap();
    assert_eq!(engines(&cfg), "work codex\n");
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
    let cfg = config(d.path());
    sets(&cfg, &[("one", "work claude"), ("two", "work codex")]);
    use_set(&cfg, "one");
    use_set(&cfg, "two");
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
    assert_eq!(engines(&cfg), "work claude\n");
    // GitHub may have closed it despite the lost response, or a person may
    // close it manually. The latest reopen event distinguishes both cases.
    let mut reopened = github("sam", "use back", None);
    cycle(&mut reopened, "reopened-after-ambiguous-close");
    pass(&mut reopened, &cfg).unwrap();
    assert_eq!(engines(&cfg), "work codex\n");
    pass(&mut reopened, &cfg).unwrap(); // same open cycle: only a reply retry
    assert_eq!(engines(&cfg), "work codex\n");
}

#[test]
fn reply_receipt_write_failure_and_an_intervening_cli_do_not_repeat_back() {
    let d = tempfile::tempdir().unwrap();
    let cfg = config(d.path());
    sets(
        &cfg,
        &[
            ("one", "work claude"),
            ("two", "work codex"),
            ("three", "work codex over 80 claude"),
        ],
    );
    for name in ["two", "one", "two"] {
        use_set(&cfg, name);
    }
    let state = cfg.state.join("sam--box");
    fs::create_dir_all(&state).unwrap();
    let hash: String = Sha256::digest(b"issue-one:initial")
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let blocked = state.join(format!("1-{hash}.next"));
    fs::create_dir(&blocked).unwrap();
    let mut gh = github("sam", "use back", None);
    assert!(pass(&mut gh, &cfg).is_err());
    assert_eq!(engines(&cfg), "work claude\n");
    use_set(&cfg, "three");
    fs::remove_dir(blocked).unwrap();
    pass(&mut gh, &cfg).unwrap();
    assert_eq!(engines(&cfg), "work codex over 80 claude\n");
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
            settings: burrow_settings::Config {
                overlay: dir.path().join("overlay.json"),
                ..Default::default()
            },
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
