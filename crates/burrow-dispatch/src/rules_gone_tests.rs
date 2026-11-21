//! The rules for a runner whose registration GitHub deleted.

use std::collections::BTreeMap;

use burrow_watchdog::words::clock;

use super::super::{Decision, Move, decide};
use super::{G, Q, T, restarts, seen, snap};
use crate::registration::{Registered, Registration};
use crate::snapshot::Seen;
use crate::snapshot::tests::config;
use crate::state::State;
use crate::world::Unit;

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

    // Started by a timer too, which sorts first: never in the stop set,
    // which polkit allows only its own path unit and it, and named in its
    // wait; an inactive one isn't.
    let mut s = gone("active", &[]);
    let up = s.triggers["ci.path"].clone();
    s.triggers.insert("a-recovery.timer".into(), up);
    s.triggers.insert(
        "b.path".into(),
        Unit {
            state: "inactive".into(),
            ..Unit::default()
        },
    );
    let d = decide(&cfg, &snap(s), &st, &none, T + Q);
    assert_eq!(halts(&d), [vec!["ci.path", "ci"]]);
    assert!(
        d.items["runner/ci"]
            .phase
            .ends_with("until it's registered again; a-recovery.timer starts it too, which dispatch doesn't stop"),
        "{}",
        d.items["runner/ci"].phase
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
