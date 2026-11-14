//! Every open issue listed, and the drain's questions to GitHub, at most
//! once a gap however many events and turns' ends come in it: the GitHub
//! stand-in's log counts what each burst cost.

use std::collections::BTreeSet;
use std::fs;

use serde_json::{Value, json};

use super::drain::DRAIN_GAP;
use super::scope::{LISTED, LISTING_GAP};
use super::sorting_tests::{Box, issue, on_github};
use crate::now;

const N: u64 = 6;
const LISTING: &str = "api repos/sam/box/issues?state=open";
const PICKED: &str = "api repos/sam/box/issues/7\n";

fn found(n: u64, labels: &[&str]) -> Value {
    let mut i = issue(n, "sam", labels);
    i["repository_url"] = json!("https://api.github.com/repos/sam/box");
    i["created_at"] = json!("2026-10-01T00:00:00Z");
    i["state"] = json!("open");
    i
}

/// The steward relabelling issue `n`: its event's pass looks at it alone.
fn relabelled(b: &Box, n: u64) {
    let mut p = b.pass();
    p.scope = Some(BTreeSet::from([n]));
    p.run("sam/box").unwrap();
    p.finish("sam/box").unwrap();
}

/// A turn's end: its job's repo pass (proj run --kick), then the drain's own
/// pass (a ready issue was waiting).
fn turn_ended(b: &Box) {
    let mut p = b.pass();
    p.run("sam/box").unwrap();
    p.finish("sam/box").unwrap();
    let mut p = b.pass();
    p.drain_once().unwrap();
    p.finish("drain").unwrap();
}

fn listings(b: &Box) -> usize {
    b.log().lines().filter(|l| l.starts_with(LISTING)).count()
}

// A burst of N steward relabels and N turns' ends lists the repo's open
// issues once and asks GitHub about the drain's pick once (one drain round:
// its label goes on, and the enrolment it leaves isn't asked about again
// within the gap). What came too soon books the passes that do it again at
// the gaps' ends, and those list and ask once more.
#[test]
fn a_burst_lists_and_drains_once() {
    let mut b = Box::new();
    b.cfg.notify = None;
    let mut open = vec![found(7, &["ready"])];
    for k in 1..=N {
        let relabelled = found(200 + k, &["later"]);
        on_github(&b, &relabelled);
        open.push(relabelled);
    }
    on_github(&b, &open[0]);
    // As GitHub has them: the labelled listings don't have #7 yet (its
    // labelled event's pass starts its job), every open issue's does, and
    // once the drain's label is on, #7 says so.
    fs::write(b.path("every"), Value::from(open).to_string()).unwrap();
    fs::write(b.path("labelled"), "[]").unwrap();
    fs::write(b.path("seven"), found(7, &["ready", "codex"]).to_string()).unwrap();
    burrow_test::executable(
        b.path("hook"),
        r#"#!/bin/sh
case "$1" in
  *"issues?labels="*) cp "$TEST/labelled" "$TEST/repos_sam_box_issues" ;;
  *"issues?state=open"*) cp "$TEST/every" "$TEST/repos_sam_box_issues" ;;
  *"POST repos/sam/box/issues/7/labels"*) cp "$TEST/seven" "$TEST/repos_sam_box_issues_7" ;;
esac
"#,
    );

    for k in 1..=N {
        relabelled(&b, 200 + k);
        turn_ended(&b);
    }
    let log = b.log();
    assert_eq!(listings(&b), 1, "{log}");
    assert_eq!(log.matches(PICKED).count(), 1, "one drain round: {log}");
    assert_eq!(
        log.matches("issues/7/labels -f labels[]=codex").count(),
        1,
        "{log}"
    );
    for k in 1..=N {
        assert!(
            log.contains(&format!("api repos/sam/box/issues/{}\n", 200 + k)),
            "{log}"
        );
    }

    // Booked for the gaps' ends: the repo's pass, and the drain's own.
    let inbox = b.cfg.inbox.as_ref().unwrap();
    let wake: serde_json::Map<String, Value> =
        serde_json::from_slice(&fs::read(&inbox.wake).unwrap()).unwrap();
    let at = wake["sam/box"].as_i64().unwrap();
    assert!(at > now() && at <= now() + LISTING_GAP, "{wake:?}");
    let drain: Value = serde_json::from_slice(&fs::read(&inbox.drain_wake).unwrap()).unwrap();
    let at = drain[""].as_i64().unwrap();
    assert!(at > now() && at <= now() + DRAIN_GAP, "{drain}");

    // The gaps over: the next repo pass lists again, and the drain asks
    // again whether the issue it handed on still has its label.
    let long_ago = format!("{}\n", now() - LISTING_GAP.max(DRAIN_GAP));
    fs::write(inbox.state.join(LISTED).join("sam_box"), &long_ago).unwrap();
    fs::write(inbox.state.join(".drain-asked"), &long_ago).unwrap();
    turn_ended(&b);
    let log = b.log();
    assert_eq!(listings(&b), 2, "{log}");
    assert_eq!(log.matches(PICKED).count(), 2, "{log}");
}

// A ready list gone stale (its issue sorted otherwise since) is put right by
// the drain's own read of it: no pass is booked to list the repo again, and
// the next round doesn't ask about it.
#[test]
fn the_drain_puts_a_stale_list_right() {
    let b = Box::new();
    let p = b.pass();
    p.note_ready("sam/box", &[found(7, &["ready"]), found(8, &["ready"])])
        .unwrap();
    on_github(&b, &found(7, &["later"]));
    on_github(&b, &found(8, &["ready"]));
    let mut p = b.pass();
    p.drain().unwrap();
    let log = b.log();
    assert!(log.contains(PICKED), "{log}");
    assert!(log.contains("issues/8/labels -f labels[]=codex"), "{log}");
    // Only the label's own booking: nothing to list the repo again now.
    assert!(p.wake["sam/box"] >= now() + 59, "{:?}", p.wake);
    let left =
        fs::read_to_string(b.cfg.inbox.as_ref().unwrap().state.join(".ready/sam_box")).unwrap();
    assert!(!left.contains("\"number\":7"), "{left}");
}

// With no room on the engine its list routes it to, the drain doesn't ask
// GitHub about a ready issue at all: it would wait all the same.
#[test]
fn no_room_asks_nothing() {
    let b = Box::new();
    b.pass()
        .note_ready("sam/box", &[found(7, &["ready"]), found(8, &["ready"])])
        .unwrap();
    let cfg = &b.cfg;
    let _held: Vec<fs::File> = (1..=cfg.codex_slots)
        .map(|i| {
            let dir = cfg.jobs.join(".codex-slots");
            fs::create_dir_all(&dir).unwrap();
            let f = fs::File::create(dir.join(i.to_string())).unwrap();
            f.lock().unwrap();
            f
        })
        .collect();
    b.pass().drain().unwrap();
    let state = &b.cfg.inbox.as_ref().unwrap().state;
    assert!(state.join(super::WAITING).exists());
    assert!(
        !b.log().contains("api repos/sam/box/issues/"),
        "{}",
        b.log()
    );
}

// A listing that fails holds the next off for the gap too, so an outage
// isn't asked about again on every kick; the ready list it had stays.
#[test]
fn a_failed_listing_holds_the_next_off_too() {
    let mut b = Box::new();
    b.cfg.notify = None;
    fs::write(b.path("repos_sam_box_issues"), "[]").unwrap();
    b.pass()
        .note_ready("sam/box", &[found(7, &["ready"])])
        .unwrap();
    on_github(&b, &found(7, &["ready"]));
    fs::write(b.path("fail"), "issues?state=open\n").unwrap();
    for _ in 0..3 {
        let mut p = b.pass();
        p.run("sam/box").unwrap();
        p.finish("sam/box").unwrap();
    }
    assert_eq!(listings(&b), 1, "{}", b.log());
    let state = &b.cfg.inbox.as_ref().unwrap().state;
    let list = fs::read_to_string(state.join(".ready/sam_box")).unwrap();
    assert!(list.contains("\"number\":7"), "{list}");
}

// A repo out of reach stays so, its first miss's time kept, until a pass
// lists it again in full: not one that left the listing to a later pass.
// The drain's own pass's misses count too.
#[test]
fn only_a_full_look_says_a_repo_is_reached_again() {
    let mut b = Box::new();
    b.cfg.notify = None;
    let state = b.cfg.inbox.as_ref().unwrap().state.clone();
    let marker = state.join(".unreached/sam_box");
    fs::write(b.path("repos_sam_box_issues"), "[]").unwrap();
    fs::write(b.path("fail"), "issues?state=open\n").unwrap();
    let mut p = b.pass();
    p.run("sam/box").unwrap();
    p.finish("sam/box").unwrap();
    let since = fs::read_to_string(&marker).unwrap();

    fs::write(b.path("fail"), "").unwrap();
    let mut p = b.pass();
    p.run("sam/box").unwrap();
    p.finish("sam/box").unwrap();
    assert_eq!(p.skipped, ["sam/box"]);
    assert_eq!(
        fs::read_to_string(&marker).unwrap(),
        since,
        "listing left for later"
    );

    let long_ago = format!("{}\n", now() - LISTING_GAP);
    fs::write(state.join(LISTED).join("sam_box"), &long_ago).unwrap();
    let mut p = b.pass();
    p.run("sam/box").unwrap();
    p.finish("sam/box").unwrap();
    assert!(!marker.exists(), "listed in full");

    // The drain's own pass, GitHub not saying about its pick.
    b.pass()
        .note_ready("sam/box", &[found(7, &["ready"])])
        .unwrap();
    fs::write(b.path("fail"), "repos/sam/box/issues/7\n").unwrap();
    let mut p = b.pass();
    p.drain_once().unwrap();
    p.finish("drain").unwrap();
    assert!(marker.exists(), "{}", b.log());
}
