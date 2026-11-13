//! An issue's event's pass ([`Pass::scope`]): the issues burrow-events told
//! it, each asked for by number, and nothing it keeps of the rest changed.

use std::collections::BTreeSet;
use std::fs;

use serde_json::{Value, json};

use super::drain::READY_LISTS;
use super::scope::issues_in;
use super::sorting_tests::{Box, issue};

fn scoped_issue(n: u64) -> Value {
    let mut i = issue(n, "sam", &["claude"]);
    i["repository_url"] = json!("https://api.github.com/repos/sam/box");
    i["state"] = json!("open");
    i
}

/// Issue N's record, its job `id`'s.
fn record(b: &Box, n: u64, id: &str) -> std::path::PathBuf {
    let d = b
        .cfg
        .inbox
        .as_ref()
        .unwrap()
        .state
        .join(format!("sam_box-{n}"));
    fs::create_dir_all(&d).unwrap();
    for (f, v) in [
        ("issue", format!("sam/box#{n}")),
        ("job", id.into()),
        ("answered", "0".into()),
        ("seen", "0".into()),
    ] {
        fs::write(d.join(f), format!("{v}\n")).unwrap();
    }
    d
}

// An issue's event's pass asks GitHub for that issue by number and acts on
// it, and lists none of the repo's issues: so the job of an issue it wasn't
// told about, which no list it read has, is left as it is. A pass told no
// issues lists them, a listing a label.
#[test]
fn an_issue_events_pass_asks_for_its_issue_alone() {
    let b = Box::new();
    b.job("j1", "done", "Fixed in #12.");
    let seven = b.record("j1");
    b.job("j2", "done", "Fixed in #13.");
    let eight = record(&b, 8, "j2");
    let mut now = scoped_issue(7);
    now["state"] = json!("open");
    fs::write(b.path("repos_sam_box_issues_7"), now.to_string()).unwrap();

    let mut p = b.pass();
    p.scope = Some(BTreeSet::from([7]));
    p.run("sam/box").unwrap();
    let log = b.log();
    assert!(log.contains("api repos/sam/box/issues/7\n"), "{log}");
    assert!(!log.contains("repos/sam/box/issues?"), "{log}");
    assert!(!log.contains("issues/8"), "{log}");
    // Issue 7's job answered and its issue closed; 8's untouched.
    assert!(b.posted().contains("Fixed in #12."), "{}", b.posted());
    assert!(!b.posted().contains("Fixed in #13."), "{}", b.posted());
    assert!(!seven.exists());
    assert_eq!(fs::read_to_string(eight.join("answered")).unwrap(), "0\n");

    fs::remove_file(b.path("log")).unwrap();
    b.pass().run("sam/box").unwrap();
    let log = b.log();
    assert!(
        log.contains("api repos/sam/box/issues?labels=claude&state=open&per_page=100"),
        "{log}"
    );
}

// A closed issue it's told about ends its job, as a list without it would.
#[test]
fn an_issue_events_pass_ends_a_closed_issues_job() {
    let b = Box::new();
    b.job("j1", "done", "Fixed in #12.");
    let seven = b.record("j1");
    let mut now = scoped_issue(7);
    now["state"] = json!("closed");
    fs::write(b.path("repos_sam_box_issues_7"), now.to_string()).unwrap();
    let mut p = b.pass();
    p.scope = Some(BTreeSet::from([7]));
    p.run("sam/box").unwrap();
    assert!(!seven.exists());
    assert!(!b.log().contains("repos/sam/box/issues?"), "{}", b.log());
}

// It changes only its issues' places in the repo's ready list, and keeps
// the repo's wake-up for the rest; a pass over every issue replaces both.
#[test]
fn an_issue_events_pass_keeps_what_it_has_of_the_rest() {
    let mut b = Box::new();
    b.cfg.notify = None;
    let ready = |n| issue(n, "sam", &["ready"]);
    b.pass()
        .note_ready("sam/box", &[ready(3), ready(7)])
        .unwrap();
    let mut p = b.pass();
    p.scope = Some(BTreeSet::from([7, 9]));
    p.note_ready("sam/box", &[ready(9)]).unwrap();
    let state = &b.cfg.inbox.as_ref().unwrap().state;
    let listed = || -> Vec<u64> {
        let text = fs::read(state.join(READY_LISTS).join("sam_box")).unwrap();
        let list: Vec<Value> = serde_json::from_slice(&text).unwrap();
        list.iter().filter_map(|i| i["number"].as_u64()).collect()
    };
    assert_eq!(listed(), [3, 9]);
    b.pass().note_ready("sam/box", &[ready(9)]).unwrap();
    assert_eq!(listed(), [9]);

    let wake = b.cfg.inbox.as_ref().unwrap().wake.clone();
    let soon = crate::now() + 100;
    let woken = || -> i64 {
        let text = fs::read(&wake).unwrap();
        serde_json::from_slice::<Value>(&text).unwrap()["sam/box"]
            .as_i64()
            .unwrap()
    };
    for (scope, want) in [(Some(BTreeSet::from([7])), soon), (None, soon + 400)] {
        fs::write(&wake, json!({"sam/box": soon}).to_string()).unwrap();
        let mut p = b.pass();
        p.scope = scope;
        p.looked = vec!["sam/box".into()];
        p.book("sam/box", soon + 400);
        p.finish("sam/box").unwrap();
        assert_eq!(woken(), want);
    }
}

// Absence-based cleanup of planner waits covers only the issues a pass
// asked for: a scoped pass keeps the waits of the rest (admission and
// verdict alike), retires those of its own issues that are gone or closed,
// and a full listing retires every vanished one.
#[test]
fn a_scoped_pass_keeps_the_planner_waits_of_the_rest() {
    let b = Box::new();
    let dir = b.cfg.inbox.as_ref().unwrap().state.join(".triage-waiting");
    fs::create_dir_all(&dir).unwrap();
    let wait = |n: u64| dir.join(format!("sam_box-{n}"));
    let seed = || {
        for (n, why) in [
            (7, "planner slot"),
            (8, "pending verdict"),
            (9, "planner slot"),
        ] {
            fs::write(wait(n), format!("sam/box#{n}: {why}")).unwrap();
        }
    };
    seed();
    // Scope {7, 8}: 7 closed (absent from the open list), 8 still open; 9
    // is outside the scope and not listed.
    let mut p = b.pass();
    p.scope = Some(BTreeSet::from([7, 8]));
    p.reconcile_planner_waits("sam/box", &[scoped_issue(8)])
        .unwrap();
    assert!(!wait(7).exists());
    assert!(wait(8).exists());
    assert!(wait(9).exists());
    // A full listing with only 8 retires 9 too.
    seed();
    b.pass()
        .reconcile_planner_waits("sam/box", &[scoped_issue(8)])
        .unwrap();
    assert!(!wait(7).exists());
    assert!(wait(8).exists());
    assert!(!wait(9).exists());
}

// What burrow-events tells it: numbers, one a line; anything else, or
// none, is a look at every issue.
#[test]
fn told_issues() {
    assert_eq!(issues_in("7\n12\n"), Some(BTreeSet::from([7, 12])));
    assert_eq!(issues_in(""), None);
    assert_eq!(issues_in("7\nx\n"), None);
    assert_eq!(issues_in("-1\n"), None);
}

// gh follows transferred-issue redirects. Neither a different number nor
// the same number in another repo is the requested issue, even if closed.
#[test]
fn scoped_fetches_reject_redirects_and_book_a_full_reconciliation() {
    for (repo, n) in [("else/box", 42), ("else/box", 7), ("sam/box", 42)] {
        for state in ["open", "closed"] {
            let mut b = Box::new();
            b.cfg.notify = None;
            b.job("j1", "done", "original answer");
            let saved = b.record("j1");
            let mut redirected = scoped_issue(n);
            redirected["repository_url"] = json!(format!("https://api.github.com/repos/{repo}"));
            redirected["state"] = json!(state);
            fs::write(b.path("repos_sam_box_issues_7"), redirected.to_string()).unwrap();
            let mut p = b.pass();
            p.scope = Some(BTreeSet::from([7]));
            p.run("sam/box").unwrap();
            p.finish("sam/box").unwrap();
            assert_eq!(p.unreached, ["sam/box"]);
            assert!(p.wake.contains_key("sam/box"));
            assert_eq!(fs::read_to_string(saved.join("answered")).unwrap(), "0\n");
            assert!(b.posted().is_empty());
            assert!(!b.log().contains("api -X"), "{}", b.log());
            assert!(!b.log().contains("repos/else/"), "{}", b.log());
        }
    }
}

#[test]
fn scoped_fetches_accept_repository_case() {
    let b = Box::new();
    let mut seven = scoped_issue(7);
    seven["repository_url"] = json!("https://api.github.com/repos/SAM/Box");
    fs::write(b.path("repos_sam_box_issues_7"), seven.to_string()).unwrap();
    assert_eq!(
        b.pass().asked("sam/box", BTreeSet::from([7])).unwrap(),
        [seven]
    );
}

#[test]
fn scoped_success_keeps_failure_age_and_notification_until_a_full_success() {
    let mut b = Box::new();
    let notify = b.path("notify");
    burrow_test::executable(
        &notify,
        format!(
            "#!/bin/sh\necho \"$*\" >>'{}'\n",
            b.path("notices").display()
        ),
    );
    b.cfg.notify = Some(notify);
    let marker = b
        .cfg
        .inbox
        .as_ref()
        .unwrap()
        .state
        .join(".unreached/sam_box");
    fs::create_dir_all(marker.parent().unwrap()).unwrap();
    let first = format!("{}\n", crate::now() - 7200);
    fs::write(&marker, &first).unwrap();
    fs::write(
        b.path("repos_sam_box_issues_7"),
        scoped_issue(7).to_string(),
    )
    .unwrap();
    // A scoped success cannot verify issue 8 or a failed repo listing.
    for failed in [false, true, false] {
        let mut p = b.pass();
        p.scope = Some(BTreeSet::from([7]));
        p.looked.push("sam/box".into());
        if failed {
            p.miss("sam/box");
        }
        p.finish("sam/box").unwrap();
        assert_eq!(fs::read_to_string(&marker).unwrap(), first);
        let notices = fs::read_to_string(b.path("notices")).unwrap();
        assert!(notices.contains("open proj-inbox"));
        assert!(!notices.contains("close proj-inbox"));
    }
    let mut p = b.pass();
    p.looked.push("sam/box".into());
    p.miss("sam/box");
    p.finish("sam/box").unwrap();
    assert_eq!(fs::read_to_string(&marker).unwrap(), first);
    let mut p = b.pass();
    p.looked.push("sam/box".into());
    p.finish("sam/box").unwrap();
    assert!(!marker.exists());
    assert!(
        fs::read_to_string(b.path("notices"))
            .unwrap()
            .contains("close proj-inbox")
    );
}
