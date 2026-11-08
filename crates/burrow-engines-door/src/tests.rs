use super::*;
use burrow_github::fake::{Fake, ok};
use serde_json::json;
use std::fs;

fn github(author: &str, body: &str, editor: Option<&str>) -> Fake {
    let mut gh = Fake::default();
    let edits = editor
        .map(|e| vec![json!({"editor":{"login":e},"deletedBy":null})])
        .unwrap_or_default();
    gh.on(
        "GET",
        "repos/sam/box/issues",
        vec![ok(200, json!([{"number":1,"user":{"login":author}}]), &[])],
    )
    .on(
        "POST",
        "graphql",
        vec![ok(
            200,
            json!({"data":{"repository":{"issue":{
                "author":{"login":author},"title":"engines","body":body,
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
    fs::write(
        d.path().join("roles.d/next"),
        "work@sam/code codex:chosen\n",
    )
    .unwrap();
    fs::write(
        d.path().join("roles.d/bad"),
        "# comment\njudge old-format\n",
    )
    .unwrap();
    fs::write(&roles, "work claude\n").unwrap();
    let cfg = Config {
        repo: "sam/box".into(),
        label: "engines".into(),
        authors: vec!["sam".into()],
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
    assert!(!comment.contains("chosen"));
    assert_eq!(gh.asked("PATCH").len(), 1);
    for body in [
        "use bad",
        "use absent",
        "use ../first",
        "use first\nuse next",
    ] {
        let mut gh = github("sam", body, None);
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
        authors: vec!["sam".into()],
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
    pass(&mut gh, &cfg).unwrap();
    assert_eq!(fs::read_to_string(&cfg.roles).unwrap(), "work codex\n");
}
