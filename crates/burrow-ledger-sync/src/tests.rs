use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;

use burrow_github::fake::{Fake, ok};
use serde_json::{Value, json};

use super::*;

/// A cn that notes its calls and exits as scripted per repo (default 0).
#[derive(Default)]
struct Scripted {
    calls: RefCell<Vec<String>>,
    exits: HashMap<&'static str, i32>,
}

impl Cn for Scripted {
    fn run(&self, args: &[&str]) -> Result<Option<i32>, String> {
        self.calls.borrow_mut().push(args.join(" "));
        Ok(Some(
            args.get(1)
                .and_then(|r| self.exits.get(r))
                .copied()
                .unwrap_or(0),
        ))
    }
}

fn config(dir: &std::path::Path, owners: &[&str]) -> Config {
    Config {
        cn: "cn".into(),
        ledger: dir.join("ledger"),
        wake: dir.join("wake.json"),
        owners: owners.iter().map(|o| (*o).to_owned()).collect(),
    }
}

fn listing(repos: &[&str]) -> Value {
    let nodes: Vec<Value> = repos
        .iter()
        .map(|r| json!({ "nameWithOwner": r }))
        .collect();
    json!({ "data": { "repositoryOwner": { "repositories": {
        "nodes": nodes, "pageInfo": { "hasNextPage": false, "endCursor": null } } } } })
}

fn booked(dir: &std::path::Path) -> Value {
    serde_json::from_slice(&fs::read(dir.join("wake.json")).unwrap()).unwrap()
}

fn gh(pages: Vec<Value>) -> Fake {
    let mut f = Fake::default();
    f.on(
        "POST",
        "graphql",
        pages.into_iter().map(|p| ok(200, p, &[])).collect(),
    );
    f
}

#[test]
fn the_ledger_is_set_up_only_until_its_anchor_exists() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path(), &[]);
    let cn = Scripted::default();
    pass(&cfg, &mut gh(vec![]), &cn, Some("sam/r"), &|| 0).unwrap();
    assert_eq!(*cn.calls.borrow(), ["init", "sync sam/r"]);
    fs::create_dir(&cfg.ledger).unwrap();
    fs::write(cfg.ledger.join("anchor"), "").unwrap();
    let cn = Scripted::default();
    pass(&cfg, &mut gh(vec![]), &cn, Some("sam/r"), &|| 0).unwrap();
    assert_eq!(*cn.calls.borrow(), ["sync sam/r"]);
}

#[test]
fn a_failed_set_up_syncs_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path(), &[]);
    struct Init;
    impl Cn for Init {
        fn run(&self, args: &[&str]) -> Result<Option<i32>, String> {
            assert_eq!(args, ["init"]);
            Ok(Some(1))
        }
    }
    // Init asserts it is never asked to sync.
    assert!(pass(&cfg, &mut gh(vec![]), &Init, Some("sam/r"), &|| 0).is_err());
}

#[test]
fn exit_75_books_the_repo_five_minutes_on_and_is_no_failure() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path(), &[]);
    let cn = Scripted {
        exits: HashMap::from([("sam/R", 75)]),
        ..Default::default()
    };
    pass(&cfg, &mut gh(vec![]), &cn, Some("sam/R"), &|| 1000).unwrap();
    assert_eq!(booked(dir.path()), json!({ "sam/r": 1300 }));
}

#[test]
fn any_other_exit_fails_and_books_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path(), &[]);
    let cn = Scripted {
        exits: HashMap::from([("sam/r", 1)]),
        ..Default::default()
    };
    assert!(pass(&cfg, &mut gh(vec![]), &cn, Some("sam/r"), &|| 0).is_err());
    assert!(!dir.path().join("wake.json").exists());
}

#[test]
fn a_booking_that_cannot_be_written_fails_and_leaves_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path(), &[]);
    let cn = Scripted {
        exits: HashMap::from([("sam/r", 75)]),
        ..Default::default()
    };
    for bad in ["", "  ", "[]", "not json", r#"{"a": 1} {"b": 2}"#] {
        fs::write(&cfg.wake, bad).unwrap();
        assert!(
            pass(&cfg, &mut gh(vec![]), &cn, Some("sam/r"), &|| 0).is_err(),
            "{bad:?}"
        );
        assert_eq!(fs::read_to_string(&cfg.wake).unwrap(), bad);
    }
}

#[test]
fn a_booking_for_another_repo_stays() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path(), &[]);
    fs::write(&cfg.wake, r#"{"sam/old": 1}"#).unwrap();
    let cn = Scripted {
        exits: HashMap::from([("sam/r", 75)]),
        ..Default::default()
    };
    pass(&cfg, &mut gh(vec![]), &cn, Some("sam/r"), &|| 1000).unwrap();
    assert_eq!(booked(dir.path()), json!({ "sam/old": 1, "sam/r": 1300 }));
}

#[test]
fn one_repo_failing_does_not_stop_the_rest_but_fails_the_pass() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path(), &["sam", "eve"]);
    let cn = Scripted {
        exits: HashMap::from([("sam/a", 1), ("sam/b", 75)]),
        ..Default::default()
    };
    let mut api = gh(vec![
        listing(&["sam/a", "sam/b", "sam/c"]),
        listing(&["eve/d"]),
    ]);
    assert!(pass(&cfg, &mut api, &cn, None, &|| 0).is_err());
    assert_eq!(
        *cn.calls.borrow(),
        [
            "init",
            "sync sam/a",
            "sync sam/b",
            "sync sam/c",
            "sync eve/d"
        ]
    );
    assert_eq!(booked(dir.path()), json!({ "sam/b": 300 }));
}

#[test]
fn an_owner_that_cannot_be_listed_fails_the_pass_not_the_other_owners() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path(), &["sam", "eve"]);
    let cn = Scripted::default();
    let mut api = Fake::default();
    api.on(
        "POST",
        "graphql",
        vec![
            ok(200, json!({ "errors": [{ "message": "boom" }] }), &[]),
            ok(200, listing(&["eve/d"]), &[]),
        ],
    );
    assert!(pass(&cfg, &mut api, &cn, None, &|| 0).is_err());
    assert_eq!(*cn.calls.borrow(), ["init", "sync eve/d"]);
}

#[test]
fn a_clean_full_pass_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path(), &["sam"]);
    let cn = Scripted::default();
    pass(&cfg, &mut gh(vec![listing(&["sam/a"])]), &cn, None, &|| 0).unwrap();
    assert_eq!(*cn.calls.borrow(), ["init", "sync sam/a"]);
}

#[test]
fn listing_follows_pages_and_asks_for_source_repos_not_archived() {
    let page = |repos: &[&str], more: bool| {
        let mut p = listing(repos);
        p["data"]["repositoryOwner"]["repositories"]["pageInfo"] =
            json!({ "hasNextPage": more, "endCursor": "c1" });
        p
    };
    let mut api = gh(vec![page(&["sam/a"], true), page(&["sam/b"], false)]);
    assert_eq!(repos(&mut api, "sam").unwrap(), ["sam/a", "sam/b"]);
    let asked = api.asked("POST");
    let vars = &asked[1].2.as_ref().unwrap()["variables"];
    assert_eq!(vars["after"], "c1");
    let q = asked[0].2.as_ref().unwrap()["query"].as_str().unwrap();
    assert!(q.contains("isArchived: false") && q.contains("isFork: false"));
}

#[test]
fn the_retry_is_five_minutes_after_cn_returns_not_after_the_pass_began() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path(), &[]);
    let clock = std::cell::Cell::new(1000);
    struct Slow<'a>(&'a std::cell::Cell<u64>);
    impl Cn for Slow<'_> {
        fn run(&self, args: &[&str]) -> Result<Option<i32>, String> {
            self.0.set(self.0.get() + 600);
            Ok(Some(if args[0] == "sync" { 75 } else { 0 }))
        }
    }
    pass(&cfg, &mut gh(vec![]), &Slow(&clock), Some("sam/r"), &|| {
        clock.get()
    })
    .unwrap();
    assert_eq!(booked(dir.path()), json!({ "sam/r": 2200 + 300 }));
}

#[test]
fn a_repo_named_with_a_leading_hyphen_reaches_cn() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path(), &[]);
    let cn = Scripted::default();
    sync_repo(&cfg, &cn, "sam/-r", &|| 0).unwrap();
    assert_eq!(*cn.calls.borrow(), ["sync sam/-r"]);
}
