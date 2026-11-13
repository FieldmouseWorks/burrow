//! `first` from an author: drained ahead of every priority, its job ahead
//! in line for a slot, and not parked by `later`; from anyone else, nothing
//! (first.rs).

use std::fs;

use serde_json::{Value, json};

use super::sorting_tests::{Box, issue, on_github};
use crate::job::Job;
use crate::pick::{Class, Slot, Waiter, pick};

fn found(n: u64, labels: &[&str], made: &str) -> Value {
    found_in("sam/box", n, labels, made)
}

fn found_in(repo: &str, n: u64, labels: &[&str], made: &str) -> Value {
    let mut i = issue(n, "sam", labels);
    i["repository_url"] = json!(format!("https://api.github.com/repos/{repo}"));
    i["html_url"] = json!(format!("https://github.com/{repo}/issues/{n}"));
    i["created_at"] = json!(made);
    i
}

/// GitHub's record that `by` put `first` on issue `n` of sam/box.
fn labelled_first(b: &Box, n: u64, by: &str) {
    labelled_first_in(b, "sam/box", n, by);
}

fn labelled_first_in(b: &Box, repo: &str, n: u64, by: &str) {
    fs::write(
        b.path(&format!("repos_{}_issues_{n}_events", repo.replace('/', "_"))),
        json!([
            {"event": "labeled", "label": {"name": "ready"}, "actor": burrow_github::fake::actor("sam")},
            {"event": "labeled", "label": {"name": "first"}, "actor": burrow_github::fake::actor(by)},
        ])
        .to_string(),
    )
    .unwrap();
}

/// A box whose jobs start: a project to run in, and a systemd-run that
/// starts nothing.
fn starting() -> Box {
    let mut b = Box::new();
    b.cfg.kick.clear();
    b.cfg.notify = None;
    b.cfg.projects = b.path("projects");
    for p in ["box", "game"] {
        fs::create_dir_all(b.cfg.projects.join(p)).unwrap();
    }
    b.cfg.tools.systemd_run = "true".into();
    b.cfg.tools.systemctl = "false".into();
    b
}

/// What the drain hands on next of these, as each repo's pass lists them.
fn drained(b: &Box, items: &[Value]) -> String {
    for i in items {
        on_github(b, i);
    }
    b.pass().note_ready("sam/box", items).unwrap();
    b.pass().drain().unwrap();
    let log = b.log();
    log.lines()
        .find(|l| l.contains("/labels -f labels[]=codex"))
        .unwrap_or_default()
        .to_owned()
}

// An author's first goes ahead of an older `priority: high` issue, `later`
// or not; anyone else's `first` changes nothing.
#[test]
fn an_authors_first_is_drained_ahead_of_every_priority() {
    for (labels, by, ahead) in [
        (&["ready", "first"][..], "sam", true),
        (&["later", "first"][..], "sam", true),
        (&["ready", "first"][..], "workers[bot]", false),
        (&["ready", "first"][..], "mallory", false),
    ] {
        let b = Box::new();
        let high = found(4, &["ready", "priority: high"], "2026-01-01T00:00:00Z");
        let marked = found(9, labels, "2026-11-01T00:00:00Z");
        labelled_first(&b, 9, by);
        let next = drained(&b, &[high, marked]);
        let want = if ahead { "issues/9/" } else { "issues/4/" };
        assert!(next.contains(want), "{labels:?} by {by}: {}", b.log());
    }
}

// One GitHub won't say who labelled is noted as not first, and its repo
// gets another look.
#[test]
fn an_unknown_labeller_is_not_first() {
    let b = Box::new();
    let high = found(4, &["ready", "priority: high"], "2026-01-01T00:00:00Z");
    let marked = found(9, &["ready", "first"], "2026-11-01T00:00:00Z");
    assert!(
        drained(&b, &[high, marked]).contains("issues/4/"),
        "{}",
        b.log()
    );
}

// The job of a drained issue an author marked first goes as first, ahead in
// line of the older normal ones for the next free slot; one the workers App
// marked waits its turn. `later` doesn't park an author's first. (In a
// pipeline repo's project it goes as pipeline work, ahead of both, anyway.)
#[test]
fn a_drained_first_job_goes_ahead_for_a_slot() {
    for (labels, by, first) in [
        (&["codex", "first"][..], "sam", true),
        (&["codex", "first", "later"][..], "sam", true),
        (&["codex", "first"][..], "workers[bot]", false),
        (&["codex"][..], "sam", false),
    ] {
        let b = starting();
        let i = found_in("sam/game", 9, labels, "2026-11-01T00:00:00Z");
        on_github(&b, &i);
        labelled_first_in(&b, "sam/game", 9, by);
        let d = b.cfg.inbox.as_ref().unwrap().state.join("sam_game-9");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("drained"), "sam/game#9\n").unwrap();
        b.pass().new_issues("sam/game", &[i]).unwrap();
        let id =
            crate::read(&d.join("job")).unwrap_or_else(|| panic!("{labels:?} by {by}: no job"));
        let priority = crate::read(&Job::new(&b.cfg, &id).turn("1", "priority"));
        assert_eq!(
            priority.as_deref(),
            Some(if first { "first" } else { "normal" }),
            "{labels:?} by {by}"
        );
    }
    // With every slot busy, the one freed goes to the first job, though the
    // normal one waited longer.
    let b = Box::new();
    let busy = Slot::Held {
        class: Some(Class::Normal),
        project: "game".into(),
    };
    let mut slots = vec![busy; b.cfg.max_jobs];
    slots[0] = Slot::Free;
    let line = [
        Waiter {
            name: "1-200-first".into(),
            class: Class::First,
            project: "box".into(),
        },
        Waiter {
            name: "2-100-older".into(),
            class: Class::Normal,
            project: "box".into(),
        },
    ];
    assert_eq!(pick(&b.cfg, &slots, &line), [(0, 1)]);
}

// `later` parks an issue whose `first` someone other than an author put on.
#[test]
fn later_parks_a_first_no_author_put_on() {
    for (by, parks) in [("sam", false), ("workers[bot]", true)] {
        let b = starting();
        let i = found(9, &["codex", "first", "later"], "2026-11-01T00:00:00Z");
        on_github(&b, &i);
        labelled_first(&b, 9, by);
        b.pass().new_issues("sam/box", &[i]).unwrap();
        let d = b.cfg.inbox.as_ref().unwrap().state.join("sam_box-9");
        assert_eq!(!d.join("job").exists(), parks, "{by}: {}", b.log());
    }
}

// Among an author's first issues the oldest goes first, whatever their
// priority or repo; the rest keep priority, pipeline and age.
#[test]
fn first_issues_go_oldest_first_among_themselves() {
    let b = Box::new();
    let t = b.cfg.inbox.as_ref().unwrap().triage.clone().unwrap();
    let mark = |mut i: Value| {
        i[super::first::MARKED] = json!(true);
        i
    };
    let items = [
        mark(found_in(
            "sam/box",
            1,
            &["ready", "first", "priority: high"],
            "2026-05-01",
        )),
        mark(found_in(
            "sam/game",
            2,
            &["later", "first", "priority: low"],
            "2026-03-01",
        )),
        mark(found_in("sam/box", 3, &["ready", "first"], "2026-04-01")),
        found_in("sam/box", 4, &["ready", "priority: high"], "2026-01-01"),
        found_in("sam/game", 5, &["ready", "priority: high"], "2026-02-01"),
    ];
    let order: Vec<String> = b
        .pass()
        .ranked(&t, &items)
        .into_iter()
        .map(|c| format!("{}#{}", c.repo, c.n))
        .collect();
    assert_eq!(
        order,
        [
            "sam/game#2",
            "sam/box#3",
            "sam/box#1",
            "sam/box#4",
            "sam/game#5"
        ]
    );
}

// With planning on, an author's first issue in `later` is planned as a
// ready one is, so it can start; one the workers App marked stays sorted.
#[test]
fn a_later_first_issue_reaches_the_planner() {
    for (by, planned) in [("sam", true), ("workers[bot]", false)] {
        let mut b = Box::new();
        b.cfg.inbox.as_mut().unwrap().planning = true;
        let t = b.cfg.inbox.as_ref().unwrap().triage.clone().unwrap();
        let i = found(9, &["later", "first"], "2026-11-01T00:00:00Z");
        on_github(&b, &i);
        labelled_first(&b, 9, by);
        let sorted = b.pass().sorted("sam/box", &t, &i, &Default::default());
        assert_eq!(!sorted, planned, "{by}");
    }
}
