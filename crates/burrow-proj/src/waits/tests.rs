// What proj tells the stall watchdog a job waits on, from what rewatch would
// find of its latest turn: a turn in line or running is watched to its
// deadline (one waiting in line inside its run as one in line for a slot
// is), one that ended without saying what comes next is due at once, one that
// failed once proj-inbox has had time to resume it, an await to its deadline past proj-watch's, and a question for a
// person, a finished job, a stopped turn and an --owned job's end aren't
// watched at all. And what a running turn waits in line for: only a waiter
// that still holds its file, so one that died mid-wait never stops the
// turn's clock.

use std::fs::{self, File};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use burrow_watchdog::watch::Item;
use serde_json::json;

use super::{Facts, job_wait, waiting};
use crate::config::Config;
use crate::config::tests::example;

/// The facts, one "KEY VALUE..." each (an await's or a wait's split again
/// at its first space: what it waits for keeps its own).
fn facts(lines: &[&str]) -> Facts {
    let mut f = Facts::default();
    for l in lines {
        let (k, v) = l.split_once(' ').unwrap();
        let pair = || {
            let (a, b) = v.split_once(' ').unwrap();
            (a.to_owned(), b.to_owned())
        };
        match k {
            "id" => f.id = v.into(),
            "turn" => f.turn = v.into(),
            "state" => f.state = v.into(),
            "owned" => f.owned = true,
            "unstick" => f.unstick = true,
            "stopped" => f.stopped = true,
            "end" => f.end = Some(v.into()),
            "link" => f.link = Some(v.into()),
            "queued" => f.queued = v.parse().unwrap(),
            "slot" => f.slot = v.parse().unwrap(),
            "ended" => f.ended = v.parse().unwrap(),
            "deadline" => f.deadline = v.parse().unwrap(),
            "wait" => f.wait = Some(pair()),
            "await" => f.awaits.push(pair()),
            _ => panic!("no such fact: {k}"),
        }
    }
    f
}

const BASE: [&str; 6] = [
    "id web-1",
    "turn 2",
    "queued 1000",
    "slot 2000",
    "ended 3000",
    "deadline 5600",
];

/// The wait for BASE and more, which must hold of `want`.
fn case(more: &[&str], want: impl Fn(&Item) -> bool) {
    let cfg: Config = serde_json::from_value(example()).unwrap();
    let all: Vec<&str> = BASE.iter().chain(more).copied().collect();
    let got = job_wait(&cfg, &facts(&all)).unwrap_or_else(|| panic!("nothing from {more:?}"));
    assert!(want(&got), "{got:?} from {more:?}");
    // Always the watchdog's whole form.
    let keys: Vec<String> = json!(got).as_object().unwrap().keys().cloned().collect();
    assert_eq!(
        keys,
        [
            "claude", "due", "kick", "line", "link", "phase", "since", "unstick", "what"
        ]
    );
}

/// Nothing to watch for these facts alone.
fn none(lines: &[&str]) {
    let cfg: Config = serde_json::from_value(example()).unwrap();
    assert_eq!(job_wait(&cfg, &facts(lines)), None, "{lines:?}");
}

fn none_more(more: &[&str]) {
    none(&BASE.iter().chain(more).copied().collect::<Vec<_>>());
}

/// Out of usage, no turn takes a slot till it resets (usage.rs): a turn in
/// line is due a stall's time after that, not after it came, and says why.
/// From 2026-11-01 to 11-04 every turn was refused, and each went to the
/// stuck issue as an error of its own.
#[test]
fn out_of_usage_a_turn_in_line_is_due_from_the_reset() {
    let cfg: Config = serde_json::from_value(example()).unwrap();
    let mut f = facts(
        &BASE
            .iter()
            .chain(&["state queued"])
            .copied()
            .collect::<Vec<_>>(),
    );
    f.spent = Some(crate::usage::Spent {
        owner: false,
        until: 50_000,
        said: "You've hit your monthly spend limit".into(),
        job: "web-1".into(),
        at: 900,
        back: None,
        admission: None,
        told: 900,
        last: Some(900),
    });
    let w = job_wait(&cfg, &f).unwrap();
    assert_eq!((w.since, w.due), (1000.0, 57_200.0));
    assert!(w.line);
    assert!(
        w.phase
            .starts_with("turn 2 is waiting in line: Claude is out of usage until ")
            && w.phase
                .ends_with("(\"You've hit your monthly spend limit\")"),
        "{}",
        w.phase
    );
    // One that came after the reset's own stall is held to its own.
    f.queued = 60_000;
    assert_eq!(job_wait(&cfg, &f).unwrap().due, 67_200.0);
    // Back sooner than its reset: a stall from then, and said so.
    f.queued = 1000;
    f.spent.as_mut().unwrap().back = Some(20_000);
    let w = job_wait(&cfg, &f).unwrap();
    assert_eq!(w.due, 27_200.0);
    assert!(
        w.phase
            .starts_with("turn 2 is waiting in line for a slot, which usage was out for till "),
        "{}",
        w.phase
    );
    // Back after it: from the reset.
    f.spent.as_mut().unwrap().back = Some(70_000);
    assert_eq!(job_wait(&cfg, &f).unwrap().due, 57_200.0);
}

#[test]
fn waits() {
    // In line: watched for stallAfterMinutes from when it was started.
    case(&["state queued"], |w| {
        w.since == 1000.0
            && w.due == 8200.0
            && w.kick.is_none()
            && w.line
            && w.what == "proj job web-1"
            && w.phase == "turn 2 is waiting in line for a slot"
    });
    // An --owned job's turns wait in line like anyone's.
    case(&["state queued", "owned 1"], |w| {
        w.due == 8200.0 && !w.unstick
    });
    // An --unstick job's say so, in line for its own slot or at work: the
    // watchdog tells the owner of its stall, as the stuck issue would only
    // hand it to itself.
    case(&["state queued", "unstick 1"], |w| {
        w.due == 8200.0
            && w.line
            && w.unstick
            && w.phase
                == "turn 2 is waiting for the --unstick slot, which another --unstick turn holds"
    });
    case(&["state running", "unstick 1"], |w| {
        w.due == 6200.0 && w.unstick
    });
    // Running: to its deadline, and GRACE for proj to have stopped it.
    case(&["state running"], |w| {
        w.since == 2000.0 && w.due == 6200.0 && !w.line && w.phase == "turn 2 is running"
    });
    // No timeout: nothing to hold it to.
    none(&["id web-1", "turn 2", "slot 2000", "state running"]);
    // Waiting in line inside its run (its deadline moving on meanwhile):
    // watched as a turn in line is, from when it started waiting, and says
    // for what.
    case(
        &["state running", "wait 2500 the heavy-run lock (gate)"],
        |w| {
            w.since == 2500.0
                && w.due == 9700.0
                && !w.line
                && w.phase == "turn 2 is running, and waiting in line: the heavy-run lock (gate)"
        },
    );
    let cfg: Config = serde_json::from_value(example()).unwrap();
    let w = job_wait(
        &cfg,
        &facts(&[
            "id web-1",
            "turn 2",
            "slot 2000",
            "state running",
            "wait 2500 a lock",
        ]),
    )
    .unwrap();
    assert!(w.since == 2500.0 && w.due == 9700.0, "{w:?}");
    // One whose time can't be read is nothing to watch.
    none_more(&["state running", "wait soon a lock"]);
    // Ended, a wait it left is nothing to watch.
    none_more(&["state done", "end done", "wait 2500 a lock"]);

    // Ended without saying what comes next, even when asked again: due at once.
    case(&["state done", "end undeclared"], |w| {
        w.since == 3000.0
            && w.due == 3000.0
            && w.kick.is_none()
            && w.phase.contains("without saying what comes next")
    });
    // Ended in a failure, or an error: due at once too, and says which.
    case(&["state timed out after 4h"], |w| {
        w.since == 3000.0 && w.due == 3120.0 && w.phase.contains("ended (timed out after 4h)")
    });
    case(&["state error", "end done"], |w| {
        w.due == 3120.0 && w.phase.contains("ended (error)")
    });
    case(&["state failed"], |w| w.due == 3120.0);
    // A question for a person, a finished job, and a turn that didn't have
    // to say (a sandbox's, or one from before turns said) aren't watched.
    none_more(&["state done", "end ask"]);
    none_more(&["state done", "end done"]);
    none_more(&["state done"]);
    // Nor is a turn proj stop ended, or how an --owned job's ended: its
    // starter acts on that.
    none_more(&["state failed", "stopped 1"]);
    none_more(&["state done", "end undeclared", "stopped 1"]);
    none_more(&["state timed out after 1h", "owned 1"]);
    none_more(&["state done", "end undeclared", "owned 1"]);

    // Awaiting GitHub: the longest wait sets the deadline, past
    // proj-watch's own (awaitHours), and its repo's proj-watch is kicked
    // first.
    case(
        &[
            "state done",
            "end await",
            "await a/b#1 5000",
            "await c/d#2 4000",
        ],
        |w| {
            w.since == 4000.0
                && w.due == (4000 + 86400 + 600) as f64
                && w.kick.as_deref() == Some("proj-watch/c/d")
                && w.phase.contains("a/b#1, c/d#2")
        },
    );
    // A line whose time can't be read is passed over, not the deadline.
    case(
        &[
            "state done",
            "end await",
            "await a/b#1 5000",
            "await c/d#2 soon",
        ],
        |w| w.since == 5000.0 && w.kick.as_deref() == Some("proj-watch/a/b"),
    );
    // Its awaits gone (proj-watch resumed it, or a stop took them): nothing.
    none_more(&["state done", "end await"]);

    // What the job is about, when it says.
    case(
        &[
            "state done",
            "end undeclared",
            "link https://github.com/a/b/issues/7",
        ],
        |w| w.link.as_deref() == Some("https://github.com/a/b/issues/7"),
    );
    case(&["state queued"], |w| w.link.is_none());
}

/// A file a waiter holds locked, written at `at`, until it's dropped: the
/// waiter dying.
fn hold(path: &std::path::Path, text: &str, at: u64) -> Held {
    fs::write(path, text).unwrap();
    let f = File::options().read(true).write(true).open(path).unwrap();
    f.set_modified(UNIX_EPOCH + Duration::from_secs(at))
        .unwrap();
    f.lock().unwrap();
    Held(f)
}

/// A waiter's locked file, which unlocks as it's dropped rather than only
/// closing. A waiter is a process of its own, and its lock goes when it
/// dies; here it's a file in a test process whose other tests spawn
/// children (git, in pick's), and a child forked while the file is open
/// shares its lock until it execs, so closing the file alone can leave it
/// held a moment longer.
struct Held(File);

impl Drop for Held {
    fn drop(&mut self) {
        self.0.unlock().unwrap();
    }
}

// What a running turn waits in line for, from its wait dir: the oldest file
// a live waiter holds locked.
#[test]
fn waiting_in_line() {
    let w = tempfile::tempdir().unwrap();
    let saw = |want: Option<(i64, &str)>| {
        assert_eq!(waiting(w.path()), want.map(|(t, s)| (t, s.to_owned())));
    };
    saw(None);
    // Held: when it was written, and its first line, control characters
    // dropped.
    let a = hold(
        &w.path().join("a"),
        "the heavy-run lock\x07 (held by the gate)\nmore\n",
        1000,
    );
    saw(Some((1000, "the heavy-run lock (held by the gate)")));
    // Two held: the oldest.
    let b = hold(&w.path().join("b"), "the gate lock\n", 2000);
    saw(Some((1000, "the heavy-run lock (held by the gate)")));
    // The oldest's waiter dies mid-wait, its file left behind: no longer a
    // wait, and gone.
    drop(a);
    saw(Some((2000, "the gate lock")));
    assert!(
        !w.path().join("a").exists(),
        "a dead waiter's file was kept"
    );
    // Just made, not locked yet: neither a wait nor removed.
    fs::write(w.path().join("c"), "").unwrap();
    saw(Some((2000, "the gate lock")));
    assert!(
        w.path().join("c").exists(),
        "a waiter's new file was removed before it could lock it"
    );
    // A line too long is cut to 200 characters; an empty one is a wait all
    // the same.
    drop(b);
    let d = hold(&w.path().join("d"), &format!("{}\n", "0".repeat(300)), 3000);
    saw(Some((3000, &"0".repeat(200))));
    drop(d);
    let e = hold(&w.path().join("e"), "", 4000);
    saw(Some((4000, "something")));
    drop(e);
    // The new file, now old and still not held, goes too.
    File::options()
        .write(true)
        .open(w.path().join("c"))
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(60))
        .unwrap();
    saw(None);
    assert_eq!(fs::read_dir(w.path()).unwrap().count(), 0);
}

#[test]
fn a_codex_refusal_waits_for_its_reset_even_for_an_owned_job() {
    let cfg: Config = serde_json::from_value(example()).unwrap();
    let f = Facts {
        id: "code".into(),
        turn: "1".into(),
        owned: true,
        state: "usage limit; waiting for Codex's reset".into(),
        usage_reset: Some((1000, 100000)),
        ..Facts::default()
    };
    let wait = job_wait(&cfg, &f).unwrap();
    assert_eq!(wait.since, 1000.0);
    assert_eq!(wait.due, (100000 + crate::GRACE) as f64);
    assert_eq!(wait.kick.as_deref(), Some("proj-watch"));
    assert!(wait.phase.contains("Codex"));
    assert!(job_wait(&cfg, &Facts { stopped: true, ..f }).is_none());
}
