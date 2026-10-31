//! The queue's odd entries, and the locks the steward's sweep and this
//! service share: every entry leaves the queue answered, unless a worker is
//! still on it or the sweep took it first.

use std::fs::{self, File};
use std::thread;
use std::time::Duration;

use serde_json::json;

use crate::rig::{HEAD, Rig};

/// One that can't be read leaves the queue, an error back for it; so does
/// a directory in an entry's place, or it'd be handed out forever; and one
/// an older steward queued, with no brief in it.
#[test]
fn an_entry_it_cant_read_is_answered() {
    let rig = Rig::new();
    fs::write(rig.path("queue/codex--unread.json"), "").unwrap();
    rig.work();
    assert!(rig.names("queue").is_empty());
    assert_eq!(rig.back("codex--unread.json")["result"], "error");
    fs::create_dir(rig.path("queue/codex--dir.json")).unwrap();
    rig.review("codex--dir.json");
    assert!(rig.names("queue").is_empty());
    assert_eq!(rig.back("codex--dir.json")["result"], "error");
    let snap = rig.snapshot(5, HEAD);
    let entry = rig.queue(&snap, 5, HEAD, json!({"prompt": null}));
    rig.work();
    let back = rig.back(&entry);
    assert!(
        back["error"]
            .as_str()
            .unwrap()
            .contains("its queue entry is unreadable"),
        "{back}"
    );
    assert_eq!(
        (&back["head"], &back["base"]),
        (&json!(HEAD), &json!("main")),
        "{back}"
    );
    assert!(rig.runs().is_empty());
}

/// One for a reviewer this machine doesn't have (removed since it was
/// queued) leaves the queue unanswered, or the queue watcher would start
/// the service until systemd gave up on it; Codex's work beside it goes on.
/// One a worker from before is still on (it holds the entry's lock) stays
/// to that worker.
#[test]
fn an_entry_for_a_reviewer_it_doesnt_have_is_dropped() {
    let rig = Rig::new();
    let snap = rig.snapshot(5, HEAD);
    let entry = rig.queue(&snap, 5, HEAD, json!({}));
    for gone in ["gone", "old"] {
        rig.queue(&snap, 5, HEAD, json!({"reviewer": gone}));
    }
    let old = format!("old--{snap}.json");
    let held = File::open(rig.path("queue").join(&old)).unwrap();
    held.lock_shared().unwrap();
    rig.work();
    assert_eq!(rig.names("queue"), std::slice::from_ref(&old));
    assert_eq!(rig.names("done"), std::slice::from_ref(&entry));
    assert_eq!(rig.runs().len(), 1);
    drop(held);
    rig.work();
    assert!(rig.names("queue").is_empty());
    assert_eq!(rig.names("done"), [entry]);
    assert!(
        rig.said().contains(&format!(
            "{old}: dropped from the queue: there's no reviewer old here"
        )),
        "{}",
        rig.said()
    );
}

/// One the sweep takes off the queue (its PR moved on) while the service
/// waits for the entry's lock is never begun: no agent, no answer.
#[test]
fn one_the_sweep_takes_first_is_never_begun() {
    let rig = Rig::new();
    let snap = rig.snapshot(5, HEAD);
    let entry = rig.queue(&snap, 5, HEAD, json!({}));
    let path = rig.path("queue").join(&entry);
    let held = File::open(&path).unwrap();
    held.lock().unwrap();
    let s = rig.service();
    let name = entry.clone();
    let worker = thread::spawn(move || burrow_review::review(&s, &name));
    thread::sleep(Duration::from_millis(500));
    fs::remove_file(&path).unwrap();
    drop(held);
    worker.join().unwrap();
    assert!(
        rig.runs().is_empty() && rig.names("done").is_empty(),
        "{:?}",
        rig.names("done")
    );
}

/// And one it's under way on holds its entry's lock: the sweep leaves it.
#[test]
fn one_under_way_holds_its_lock() {
    let rig = Rig::new();
    let snap = rig.snapshot(5, HEAD);
    rig.agent(json!({"sleep": 2}));
    let entry = rig.queue(&snap, 5, HEAD, json!({}));
    let s = rig.service();
    let name = entry.clone();
    let worker = thread::spawn(move || burrow_review::review(&s, &name));
    thread::sleep(Duration::from_secs(1));
    let f = File::open(rig.path("queue").join(&entry)).unwrap();
    assert!(
        f.try_lock().is_err(),
        "the sweep could take a look under way"
    );
    drop(f);
    worker.join().unwrap();
    let back = rig.back(&entry);
    assert_eq!(
        (&back["result"], &back["head"], &back["base"]),
        (&json!("review"), &json!(HEAD), &json!("main")),
        "{back}"
    );
}

/// A repo taken off its reviewer since the look was queued isn't sent, and
/// a snapshot gone by the time its look starts (its head swept away, or the
/// cache emptied) is no review either.
#[test]
fn a_look_it_shouldnt_take_is_an_error() {
    let mut rig = Rig::new();
    let snap = rig.snapshot(5, HEAD);
    rig.cfg["reviewers"]["codex"]["off"] = json!(["sam/r"]);
    let entry = rig.queue(&snap, 5, HEAD, json!({}));
    rig.work();
    let back = rig.back(&entry);
    assert!(
        back["error"]
            .as_str()
            .unwrap()
            .contains("sam/r isn't one Codex reviews"),
        "{back}"
    );
    rig.cfg["reviewers"]["codex"]["off"] = json!(["sam/other"]);
    rig.queue(&snap, 5, HEAD, json!({}));
    rig.work();
    assert_eq!(rig.back(&entry)["result"], "review");
    rig.queue(&snap, 5, HEAD, json!({}));
    fs::remove_dir_all(rig.path("trees").join(&snap)).unwrap();
    rig.work();
    let back = rig.back(&entry);
    assert!(
        back["error"]
            .as_str()
            .unwrap()
            .contains("its snapshot is gone"),
        "{back}"
    );
    assert_eq!(rig.runs().len(), 1);
}

/// An answer back, a review or why there's none: the repo's pass is kicked,
/// since nothing goes to GitHub until that pass posts it.
#[test]
fn an_answer_kicks_its_repos_pass() {
    let rig = Rig::new();
    let snap = rig.snapshot(5, HEAD);
    let entry = rig.queue(&snap, 5, HEAD, json!({}));
    rig.queue("gone", 6, HEAD, json!({"repo": "sam/q"}));
    rig.work();
    assert_eq!(rig.back(&entry)["result"], "review");
    assert_eq!(rig.back("codex--gone.json")["result"], "error");
    let mut kicks = rig.kicks.lock().unwrap().clone();
    kicks.sort();
    assert_eq!(kicks, ["/kick/autoland/sam/q", "/kick/autoland/sam/r"]);
}
