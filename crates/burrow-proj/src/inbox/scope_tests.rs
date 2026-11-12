//! An issue's event's pass ([`Pass::scope`]): the issues burrow-events told
//! it, each asked for by number, and nothing it keeps of the rest changed.

use std::collections::BTreeSet;
use std::fs;

use serde_json::{Value, json};

use super::drain::READY_LISTS;
use super::scope::issues_in;
use super::sorting_tests::{Box, issue};

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
    let mut now = issue(7, "sam", &["claude"]);
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
    let mut now = issue(7, "sam", &["claude"]);
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

// What burrow-events tells it: numbers, one a line; anything else, or
// none, is a look at every issue.
#[test]
fn told_issues() {
    assert_eq!(issues_in("7\n12\n"), Some(BTreeSet::from([7, 12])));
    assert_eq!(issues_in(""), None);
    assert_eq!(issues_in("7\nx\n"), None);
    assert_eq!(issues_in("-1\n"), None);
}
