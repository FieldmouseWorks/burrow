use std::collections::BTreeMap;

use burrow_watchdog::watch::Item;
use burrow_watchdog::words::clock;

use super::{Decision, Move, RECENT, decide};
use crate::registration::{Registered, Registration};
use crate::snapshot::tests::{config, fresh};
use crate::snapshot::{Behind, Job, Pool, Run, Seen, Snapshot};
use crate::state::State;
use crate::world::Unit;

const T: f64 = 1_800_000_000.0;
const Q: f64 = 300.0;
const G: f64 = 120.0;

/// A runner's look, with its queued jobs by name (in their links) and time.
fn seen(state: &str, since: f64, busy: bool, queued: &[(&str, f64)]) -> Seen {
    Seen {
        repo: "sam/r".into(),
        unit: Unit {
            state: state.into(),
            since: Some(since),
            busy,
            ..Unit::default()
        },
        queued: queued
            .iter()
            .map(|(name, at)| Job {
                url: format!("https://x/{name}"),
                at: *at,
            })
            .collect(),
        ..Seen::default()
    }
}

fn snap(ci: Seen) -> Snapshot {
    Snapshot {
        runners: BTreeMap::from([("ci".into(), ci)]),
        ..Snapshot::default()
    }
}

fn restarts(d: &Decision) -> Vec<(&str, &str)> {
    d.moves
        .iter()
        .filter_map(|m| match m {
            Move::Runner { unit, verb, .. } => Some((unit.as_str(), *verb)),
            _ => None,
        })
        .collect()
}

fn cancels(d: &Decision) -> Vec<u64> {
    d.moves
        .iter()
        .filter_map(|m| match m {
            Move::Cancel { run, .. } => Some(*run),
            _ => None,
        })
        .collect()
}

fn run(id: u64, workflow: u64, sha: &str, status: &str, at: f64) -> Run {
    Run {
        id,
        url: format!("https://x/runs/{id}"),
        workflow,
        branch: "main".into(),
        sha: sha.into(),
        status: status.into(),
        attempt: 1,
        at,
    }
}

// An idle runner with work queued is watched from the pass that first
// sees it so, restarted once it has been idle `quiet` (and up that long
// itself), with a look booked `quiet` on, restarted again then, and then
// left to the watchdog, whose deadline its restarts don't move: each
// restart and the look after it have had their `quiet` and the
// handler's `gap` by then, and the watchdog kicks a fresh look first.
// Its wait names no job: their names are the workflow's.
#[test]
fn quiet_runner() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let none = BTreeMap::new();
    let mut st = State::default();
    let s = snap(seen(
        "active",
        T - 3600.0,
        false,
        &[("gate", T - 900.0), ("lint", T - 60.0)],
    ));

    let d = decide(&cfg, &s, &st, &none, T);
    assert!(d.moves.is_empty() && d.wake == Some(T + Q), "{d:?}");
    assert_eq!(d.state.idle["ci"], T);
    let it = &d.items["runner/ci"];
    assert_eq!(
        (it.since, it.due, it.kick.as_deref()),
        (T, T + 3.0 * (Q + G), Some("dispatch"))
    );
    assert_eq!(it.link.as_deref(), Some("https://x/gate"));
    assert_eq!(
        it.phase,
        format!(
            "2 jobs queued for it (the oldest since {}), and it takes none",
            clock(T - 900.0)
        )
    );
    st = d.state;

    let d = decide(&cfg, &s, &st, &none, T + Q);
    assert_eq!(restarts(&d), [("ci", "restart")]);
    assert_eq!(d.wake, Some(T + 2.0 * Q));
    assert_eq!(d.items["runner/ci"].since, T);
    st = d.state;
    st.restarts.insert("ci".into(), vec![T + Q]);

    // Freshly restarted: it gets its own `quiet` before the next.
    let s = snap(seen("active", T + Q, false, &[("gate", T - 900.0)]));
    let d = decide(&cfg, &s, &st, &none, T + Q + 10.0);
    assert!(d.moves.is_empty() && d.wake == Some(T + 2.0 * Q), "{d:?}");
    let d = decide(&cfg, &s, &st, &none, T + 2.0 * Q);
    assert_eq!(restarts(&d), [("ci", "restart")]);
    assert_eq!(d.wake, Some(T + 3.0 * Q));
    st = d.state;
    st.restarts.get_mut("ci").unwrap().push(T + 2.0 * Q);

    // Twice in RECENT: no more, and its wait falls due as it was.
    let d = decide(&cfg, &s, &st, &none, T + 3.0 * Q);
    assert!(d.moves.is_empty() && d.wake.is_none(), "{d:?}");
    assert_eq!(d.items["runner/ci"].due, T + 3.0 * (Q + G));
    // RECENT on, it may be restarted again.
    let d = decide(&cfg, &s, &st, &none, T + 2.0 * Q + RECENT + 1.0);
    assert_eq!(restarts(&d), [("ci", "restart")]);
}

// At a job, or with nothing queued for it, a runner is fine: nothing
// watched, nothing done, and its idle time starts over.
#[test]
fn busy_or_free_runner() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let st = State {
        idle: BTreeMap::from([("ci".into(), T - 3600.0)]),
        ..State::default()
    };
    for s in [
        seen("active", 0.0, true, &[("gate", T - 900.0)]),
        seen("active", 0.0, false, &[]),
    ] {
        let d = decide(&cfg, &snap(s), &st, &BTreeMap::new(), T);
        assert!(
            d.moves.is_empty() && d.items.is_empty() && d.state.idle.is_empty(),
            "{d:?}"
        );
    }
}

// A failed runner is started at once, and looked at again; one that's
// stopped, starting or stopping is only watched, and looked at again;
// none moves while a switch is in flight.
#[test]
fn runner_states() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let none = BTreeMap::new();
    let st = State::default();
    let d = decide(
        &cfg,
        &snap(seen("failed", T - 60.0, false, &[("gate", T)])),
        &st,
        &none,
        T,
    );
    assert_eq!(restarts(&d), [("ci", "start")]);
    assert_eq!(d.wake, Some(T + Q));
    assert!(
        d.items["runner/ci"]
            .phase
            .starts_with("it's failed, with 1 job queued")
    );
    for state in ["inactive", "activating", "deactivating"] {
        let d = decide(
            &cfg,
            &snap(seen(state, T - 60.0, false, &[("gate", T)])),
            &st,
            &none,
            T + Q,
        );
        assert!(
            d.moves.is_empty() && d.wake == Some(T + 2.0 * Q),
            "{state}: {d:?}"
        );
        assert!(d.items.contains_key("runner/ci"));
    }
    let mut s = snap(seen("active", 0.0, false, &[("gate", T - 900.0)]));
    s.switching = true;
    let st = State {
        idle: BTreeMap::from([("ci".into(), T - 900.0)]),
        ..State::default()
    };
    let d = decide(&cfg, &s, &st, &none, T);
    assert!(d.moves.is_empty() && d.wake == Some(T + Q), "{d:?}");
}

/// What's left to stop of a runner whose registration is gone, and why.
fn halts(d: &Decision) -> Vec<Vec<&str>> {
    d.moves
        .iter()
        .filter_map(|m| match m {
            Move::Halt { stop, .. } => Some(stop.iter().map(String::as_str).collect()),
            _ => None,
        })
        .collect()
}

/// A runner looping with its registration gone at GitHub: its path unit up,
/// itself `state`.
fn gone(state: &str, queued: &[(&str, f64)]) -> Seen {
    let mut s = seen(state, T - 3600.0, false, queued);
    s.registered = Registered::Gone(Registration {
        id: 21,
        name: "box".into(),
        scope: "sam/r".into(),
    });
    s.triggers = BTreeMap::from([(
        "ci.path".into(),
        Unit {
            state: "active".into(),
            ..Unit::default()
        },
    )]);
    s
}

// A runner GitHub has deleted the registration of is told apart from an
// idle one: never restarted however long jobs wait for it, its wait says
// its registration is gone (with nothing queued too), due once a fresh look
// has had its time, and what loops it is stopped, its path unit first.
// Not while a switch is in flight; and a look that can't tell keeps it as
// the last pass saw it, with no restart.
#[test]
fn gone_registration() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let none = BTreeMap::new();
    let st = State {
        idle: BTreeMap::from([("ci".into(), T - 3600.0)]),
        ..State::default()
    };
    let d = decide(
        &cfg,
        &snap(gone("active", &[("gate", T - 900.0)])),
        &st,
        &none,
        T,
    );
    assert!(restarts(&d).is_empty(), "{d:?}");
    assert_eq!(halts(&d), [vec!["ci.path", "ci"]]);
    assert!(d.state.idle.is_empty());
    assert_eq!(d.state.gone["ci"], T);
    let it = &d.items["runner/ci"];
    assert_eq!(
        it.phase,
        format!(
            "its registration is gone at GitHub: sam/r lists no runner 21 (box), so it takes no job until it's registered again; 1 job queued for it (the oldest since {})",
            clock(T - 900.0)
        )
    );
    assert_eq!(
        (it.since, it.due, it.link.as_deref(), it.kick.as_deref()),
        (
            T,
            T + Q + G,
            Some("https://github.com/sam/r/settings/actions/runners"),
            Some("dispatch")
        )
    );
    let st = d.state;

    // Between starts, nothing queued: its path unit alone, and the wait
    // keeps its age.
    let d = decide(&cfg, &snap(gone("inactive", &[])), &st, &none, T + Q);
    assert_eq!(halts(&d), [vec!["ci.path"]]);
    let it = &d.items["runner/ci"];
    assert_eq!((it.since, it.due), (T, T + Q + G));
    assert!(
        it.phase.ends_with("until it's registered again"),
        "{}",
        it.phase
    );

    // Stopped: nothing more to do, and still waiting to be registered.
    let mut s = gone("inactive", &[]);
    s.triggers.get_mut("ci.path").unwrap().state = "inactive".into();
    let d = decide(&cfg, &snap(s), &st, &none, T + Q);
    assert!(d.moves.is_empty() && d.wake.is_none(), "{d:?}");
    assert!(d.items.contains_key("runner/ci"));

    // A switch in flight: no move, a look later.
    let mut s = snap(gone("active", &[]));
    s.switching = true;
    let d = decide(&cfg, &s, &st, &none, T + Q);
    assert!(d.moves.is_empty() && d.wake == Some(T + 2.0 * Q), "{d:?}");

    // Its list unread now: as the last pass saw it, never restarted.
    let was = d.items.clone();
    let mut s = gone("active", &[("gate", T - 900.0)]);
    s.registered = Registered::Unknown("HTTP 502".into());
    let d = decide(&cfg, &snap(s), &st, &was, T + 2.0 * Q);
    assert!(d.moves.is_empty(), "{d:?}");
    assert_eq!(d.items, was);
    assert_eq!(d.state.gone["ci"], T);

    // Registered again: an idle runner like any other.
    let s = seen("active", T - 3600.0, false, &[("gate", T - 900.0)]);
    let d = decide(&cfg, &snap(s), &st, &none, T + 2.0 * Q);
    assert!(d.state.gone.is_empty());
    assert!(d.items["runner/ci"].phase.ends_with("and it takes none"));
}

// A repo GitHub can't be read for keeps its runners' waits as they were,
// and is a wait of its own from the first pass that couldn't, which the
// watchdog kicks this handler for.
#[test]
fn unread_repo() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let was_item = Item {
        what: "ci".into(),
        phase: "p".into(),
        since: T - 600.0,
        due: T + 300.0,
        ..Item::default()
    };
    let was = BTreeMap::from([("runner/ci".to_owned(), was_item.clone())]);
    let st = State {
        idle: BTreeMap::from([("ci".into(), T - 600.0)]),
        unread: BTreeMap::from([("sam/r".into(), T - 120.0)]),
        ..State::default()
    };
    let s = Snapshot {
        unread: BTreeMap::from([("sam/r".into(), "HTTP 502".into())]),
        ..Snapshot::default()
    };
    let d = decide(&cfg, &s, &st, &was, T);
    assert_eq!(d.items["runner/ci"], was_item);
    assert_eq!(d.state.idle["ci"], T - 600.0);
    let it = &d.items["github/sam/r"];
    assert_eq!(
        (it.since, it.due, it.kick.as_deref()),
        (T - 120.0, T - 120.0 + 1800.0, Some("dispatch"))
    );
    assert_eq!(it.phase, "its passes can't read it: HTTP 502");
    // Read again: both go.
    let d = decide(&cfg, &Snapshot::default(), &d.state, &d.items, T + 60.0);
    assert!(
        d.items.is_empty() && d.state.unread.is_empty() && d.state.idle.is_empty(),
        "{d:?}"
    );
}

// One of the fresh runners' repos not read: their wait is as the last
// pass saw it, until a pass reads them all.
#[test]
fn unread_fresh_repo() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(dir.path());
    cfg.fresh = Some(fresh(dir.path()));
    let was_item = Item {
        what: "fresh".into(),
        since: T - 600.0,
        due: T + 300.0,
        ..Item::default()
    };
    let was = BTreeMap::from([("fresh".to_owned(), was_item.clone())]);
    let st = State {
        fresh: Some(T - 600.0),
        ..State::default()
    };
    let mut s = Snapshot {
        fresh: Some(Ok(Pool::default())),
        unread: BTreeMap::from([("sam/f".into(), "HTTP 502".into())]),
        ..Snapshot::default()
    };
    let d = decide(&cfg, &s, &st, &was, T);
    assert_eq!(
        (&d.items["fresh"], d.state.fresh),
        (&was_item, Some(T - 600.0))
    );
    s.unread.clear();
    let d = decide(&cfg, &s, &st, &was, T);
    assert!(d.items.is_empty() && d.state.fresh.is_none(), "{d:?}");
}

// A watched repo's commits the running system wasn't built from wait from
// the oldest of them, due `after` later, and the watchdog kicks
// HOST-update for them; while a run that started after that commit is
// at work, they wait on it, due `run` after it started. A live repo, or
// one built from no commit, waits on nothing.
#[test]
fn deploy() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let behind = Behind {
        branch: "main".into(),
        running: "aaa".into(),
        count: 3,
        oldest: T,
        newest: "c3c3c3c3c3".into(),
    };
    let mut s = Snapshot {
        deploys: BTreeMap::from([
            ("sam/box".into(), behind),
            ("sam/kit".into(), Behind::default()),
        ]),
        update: Some((
            Unit {
                state: "inactive".into(),
                ..Unit::default()
            },
            "ok at then: deployed".into(),
        )),
        ..Snapshot::default()
    };
    let d = decide(&cfg, &s, &State::default(), &BTreeMap::new(), T + 60.0);
    assert_eq!(d.items.keys().collect::<Vec<_>>(), ["deploy/sam/box"]);
    let it = &d.items["deploy/sam/box"];
    assert_eq!(
        (it.since, it.due, it.kick.as_deref()),
        (T, T + 1800.0, Some("update"))
    );
    assert_eq!(it.what, "sam/box's main going live");
    assert_eq!(
        it.link.as_deref(),
        Some("https://github.com/sam/box/compare/aaa...main")
    );
    assert_eq!(
        it.phase,
        format!(
            "3 commits the running system wasn't built from, the oldest from {} (the newest c3c3c3c); box-update's last run: ok at then: deployed",
            clock(T)
        )
    );
    // Behind its own open PR: the longer wait, and it says why. A pass
    // that doesn't find it behind any more is back on the short one.
    s.behind = Some(405);
    let it =
        decide(&cfg, &s, &State::default(), &BTreeMap::new(), T + 60.0).items["deploy/sam/box"]
            .clone();
    assert_eq!(
        (it.since, it.due, it.kick.as_deref()),
        (T, T + 18000.0, Some("update"))
    );
    assert_eq!(
        it.phase,
        format!(
            "3 commits the running system wasn't built from, the oldest from {} (the newest c3c3c3c); behind box-update's own PR #405, until that lands; its last run: ok at then: deployed",
            clock(T)
        )
    );
    // The unit failed since that run said so: back on the short one.
    s.update.as_mut().unwrap().0.state = "failed".into();
    let it =
        decide(&cfg, &s, &State::default(), &BTreeMap::new(), T + 60.0).items["deploy/sam/box"]
            .clone();
    assert_eq!(it.due, T + 1800.0);
    s.update.as_mut().unwrap().0.state = "inactive".into();
    s.behind = None;
    // A run under way that started before the oldest commit doesn't
    // have it; one that started after does.
    s.update.as_mut().unwrap().0 = Unit {
        state: "activating".into(),
        since: Some(T - 1.0),
        busy: false,
        ..Unit::default()
    };
    assert_eq!(
        decide(&cfg, &s, &State::default(), &BTreeMap::new(), T + 60.0).items["deploy/sam/box"]
            .since,
        T
    );
    s.update.as_mut().unwrap().0.since = Some(T + 30.0);
    let it =
        decide(&cfg, &s, &State::default(), &BTreeMap::new(), T + 60.0).items["deploy/sam/box"]
            .clone();
    assert_eq!(
        (it.since, it.due, it.kick),
        (T + 30.0, T + 30.0 + 5400.0, Some(cfg.handler.clone()))
    );
    assert_eq!(
        it.phase,
        format!(
            "box-update has been putting it live since {}",
            clock(T + 30.0)
        )
    );
    // A run that has written to its logs since it started is at work: due
    // `run` after its last write, however long it has been going (a build
    // from source well past `run`). A write from before it started is an
    // earlier run's, and moves nothing.
    s.progress = Some(T + 7200.0);
    let it =
        decide(&cfg, &s, &State::default(), &BTreeMap::new(), T + 7260.0).items["deploy/sam/box"]
            .clone();
    assert_eq!(
        (it.since, it.due, it.kick),
        (T + 30.0, T + 7200.0 + 5400.0, Some(cfg.handler.clone()))
    );
    assert!(it.refresh, "a run's deadline is read between passes");
    assert_eq!(
        it.phase,
        format!(
            "box-update has been putting it live since {}, its last output at {}",
            clock(T + 30.0),
            clock(T + 7200.0)
        )
    );
    s.progress = Some(T);
    let it =
        decide(&cfg, &s, &State::default(), &BTreeMap::new(), T + 60.0).items["deploy/sam/box"]
            .clone();
    assert_eq!(it.due, T + 30.0 + 5400.0);
}

// The flake's own repo with what's live unknown (no record, or a switch
// since) waits as long as it has: from the last pass's wait for it,
// commits behind or unknown then too, so a switch never drops one, or
// from now, due `after` later, and the watchdog kicks HOST-update for
// it. While any run is at work, it waits on that, and a run that fails
// leaves it as old as it was.
#[test]
fn deploy_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let idle = |said: &str| {
        Some((
            Unit {
                state: "inactive".into(),
                ..Unit::default()
            },
            said.to_owned(),
        ))
    };
    let mut s = Snapshot {
        unknown: BTreeMap::from([("sam/box".into(), "no record".into())]),
        deploys: BTreeMap::from([("sam/kit".into(), Behind::default())]),
        update: idle("held at then: held"),
        ..Snapshot::default()
    };
    let d = decide(&cfg, &s, &State::default(), &BTreeMap::new(), T + 60.0);
    assert_eq!(d.items.keys().collect::<Vec<_>>(), ["deploy/sam/box"]);
    let it = &d.items["deploy/sam/box"];
    assert_eq!(
        (it.since, it.due, it.kick.as_deref()),
        (T + 60.0, T + 60.0 + 1800.0, Some("update"))
    );
    assert_eq!(it.what, "sam/box going live");
    assert_eq!(it.link.as_deref(), Some("https://github.com/sam/box"));
    assert_eq!(
        it.phase,
        "no record; box-update's last run: held at then: held"
    );
    assert_eq!(
        d.state.going_live,
        BTreeMap::from([("sam/box".into(), T + 60.0)])
    );
    // Commits behind the pass before: as old as they are.
    let behind = Snapshot {
        deploys: BTreeMap::from([(
            "sam/box".into(),
            Behind {
                count: 1,
                oldest: T - 600.0,
                ..Behind::default()
            },
        )]),
        update: idle("ok at then: deployed"),
        ..Snapshot::default()
    };
    let d = decide(&cfg, &behind, &State::default(), &BTreeMap::new(), T);
    assert_eq!(d.state.going_live["sam/box"], T - 600.0);
    let d = decide(&cfg, &s, &d.state, &d.items, T + 60.0);
    let it = &d.items["deploy/sam/box"];
    assert_eq!((it.since, it.due), (T - 600.0, T - 600.0 + 1800.0));
    // A run at work, whenever it started: its wait, the age kept.
    s.update.as_mut().unwrap().0 = Unit {
        state: "activating".into(),
        since: Some(T - 300.0),
        busy: false,
        ..Unit::default()
    };
    let d = decide(&cfg, &s, &d.state, &d.items, T + 90.0);
    let it = &d.items["deploy/sam/box"];
    assert_eq!(
        (it.since, it.due, it.kick.as_deref()),
        (T - 300.0, T - 300.0 + 5400.0, Some("dispatch"))
    );
    assert_eq!(d.state.going_live["sam/box"], T - 600.0);
    // Unknown, behind its own PR: the longer wait; its unit failed since,
    // the short one.
    let mut held = s.clone();
    held.behind = Some(405);
    held.update.as_mut().unwrap().0 = Unit::default();
    let e = decide(&cfg, &held, &d.state, &d.items, T + 90.0);
    assert_eq!(e.items["deploy/sam/box"].due, T - 600.0 + 18000.0);
    held.update.as_mut().unwrap().0.state = "failed".into();
    let e = decide(&cfg, &held, &d.state, &d.items, T + 90.0);
    assert_eq!(e.items["deploy/sam/box"].due, T - 600.0 + 1800.0);
    // It failed: as old as before it, not as its start.
    s.update = idle("failed at then: activation failed; reverted");
    let d = decide(&cfg, &s, &d.state, &d.items, T + 120.0);
    let it = &d.items["deploy/sam/box"];
    assert_eq!((it.since, it.due), (T - 600.0, T - 600.0 + 1800.0));
    // Its record unreadable: the wait and its age as they were.
    let unread = Snapshot {
        unread: BTreeMap::from([("sam/box".into(), "live.json: bad".into())]),
        update: idle("ok at then: up to date"),
        ..Snapshot::default()
    };
    let e = decide(&cfg, &unread, &d.state, &d.items, T + 180.0);
    assert_eq!(e.items["deploy/sam/box"], d.items["deploy/sam/box"]);
    assert_eq!(e.state.going_live["sam/box"], T - 600.0);
    // Found live: nothing waits, and the age goes.
    let live = Snapshot {
        deploys: BTreeMap::from([("sam/box".into(), Behind::default())]),
        update: idle("ok at then: up to date"),
        ..Snapshot::default()
    };
    let d = decide(&cfg, &live, &e.state, &e.items, T + 240.0);
    assert!(d.items.is_empty() && d.state.going_live.is_empty(), "{d:?}");
}

// A queued push run with a later one of its workflow live on the branch
// is cancelled, whatever the later one's state; one that has started,
// one someone ran again, or one with nothing later of its own workflow,
// is left. A run is tried once in RECENT, and the tries of a repo not
// read this time are kept.
#[test]
fn moot_runs() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let none = BTreeMap::new();
    let s = Snapshot {
        pushes: BTreeMap::from([(
            "sam/r".into(),
            vec![
                run(1, 7, "a1a1a1a1a1", "queued", T - 900.0),
                run(2, 8, "a1a1a1a1a1", "queued", T - 900.0),
                run(3, 7, "b2b2b2b2b2", "in_progress", T - 800.0),
                run(4, 7, "c3c3c3c3c3", "pending", T - 600.0),
                run(5, 9, "c3c3c3c3c3", "in_progress", T - 600.0),
                run(6, 9, "d4d4d4d4d4", "queued", T - 300.0),
                Run {
                    attempt: 2,
                    ..run(10, 8, "a0a0a0a0a0", "queued", T - 1000.0)
                },
            ],
        )]),
        ..Snapshot::default()
    };
    let d = decide(&cfg, &s, &State::default(), &none, T);
    assert_eq!(cancels(&d), [1]);
    assert!(d.items.is_empty() && d.wake.is_none(), "{d:?}");
    let Move::Cancel { repo, url, why, .. } = &d.moves[0] else {
        unreachable!()
    };
    assert_eq!((repo.as_str(), url.as_str()), ("sam/r", "https://x/runs/1"));
    assert_eq!(
        why,
        "it checks a1a1a1a on main and hasn't started, and https://x/runs/3 checks b2b2b2b, which came after it"
    );

    // Tried: not again in RECENT, and the try is kept; a repo not read
    // keeps its own.
    let st = State {
        cancels: BTreeMap::from([
            ("sam/r#1".into(), T - 60.0),
            ("sam/o#9".into(), T - 60.0),
            ("sam/o#8".into(), T - RECENT - 1.0),
        ]),
        ..State::default()
    };
    let d = decide(&cfg, &s, &st, &none, T);
    assert!(d.moves.is_empty(), "{d:?}");
    assert_eq!(
        d.state.cancels.keys().collect::<Vec<_>>(),
        ["sam/o#9", "sam/r#1"]
    );
    let d = decide(&cfg, &s, &st, &none, T + RECENT);
    assert_eq!(cancels(&d), [1]);
}

// A wait the pass couldn't read (GitHub, or the live record) still takes a
// run's progress from the unit and logs now, for the flake's own repo and
// for an input alike; a write from before the run is an earlier run's; and
// the unread alert is kept.
#[test]
fn deploy_unread_progress() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let unit = |state: &str, since: f64| Unit {
        state: state.into(),
        since: Some(since),
        ..Unit::default()
    };
    for repo in ["sam/box", "sam/kit"] {
        let key = format!("deploy/{repo}");
        let waiting = Snapshot {
            deploys: BTreeMap::from([(
                repo.into(),
                Behind {
                    count: 1,
                    oldest: T - 600.0,
                    ..Behind::default()
                },
            )]),
            update: Some((Unit::default(), "ok".into())),
            ..Snapshot::default()
        };
        let other = if repo == "sam/box" {
            "sam/kit"
        } else {
            "sam/box"
        };
        let mut base = waiting.clone();
        base.deploys.insert(other.into(), Behind::default());
        let d = decide(&cfg, &base, &State::default(), &BTreeMap::new(), T);
        // Unreadable now; a run under way, last wrote at T + 1000.
        let mut s = Snapshot {
            unread: BTreeMap::from([(repo.into(), "HTTP 502".into())]),
            update: Some((unit("activating", T - 300.0), "ok".into())),
            progress: Some(T + 1000.0),
            ..Snapshot::default()
        };
        s.deploys.insert(other.into(), Behind::default());
        let e = decide(&cfg, &s, &d.state, &d.items, T + 1100.0);
        let it = &e.items[&key];
        assert_eq!(
            (it.since, it.due, it.kick.as_deref()),
            (T - 300.0, T + 1000.0 + 5400.0, Some("dispatch")),
            "{repo}"
        );
        assert_eq!(it.what, d.items[&key].what);
        assert_eq!(it.link, d.items[&key].link);
        assert_eq!(e.state.going_live[repo], T - 600.0);
        // Only an earlier run's write: from its start.
        s.progress = Some(T - 400.0);
        let e2 = decide(&cfg, &s, &d.state, &d.items, T + 1100.0);
        assert_eq!(e2.items[&key].due, T - 300.0 + 5400.0);
        // The run it was on ended: back on HOST-update's own wait, which
        // carries on from the wait's age.
        let mut ended = s.clone();
        ended.update = Some((Unit::default(), "ok".into()));
        let e3 = decide(&cfg, &ended, &e.state, &e.items, T + 1200.0);
        let it = &e3.items[&key];
        assert_eq!(
            (it.since, it.due, it.kick.as_deref()),
            (T - 600.0, T - 600.0 + 1800.0, Some("update"))
        );
        // Not at work, not on a run: as it was.
        let e4 = decide(&cfg, &ended, &d.state, &d.items, T + 1200.0);
        assert_eq!(e4.items[&key], d.items[&key]);
    }
}
