//! What a pass makes of what it saw: the moves its rules call for, and what
//! it waits on, for the stall watchdog. Nothing here reads or acts.

use std::collections::BTreeMap;

use burrow_watchdog::MIN;
use burrow_watchdog::watch::Item;
use burrow_watchdog::words::clock;

use crate::config::Config;
use crate::snapshot::{Seen, Snapshot};
use crate::state::State;

/// Restarts of one runner are counted over this long...
pub const RECENT: f64 = 6.0 * 60.0 * MIN;
/// ...and it gets at most this many: past them, restarting isn't the fix,
/// and its wait goes on to the watchdog.
pub const RESTARTS: usize = 2;
/// A repo GitHub can't be read for this long is a stall of its own.
pub const UNREAD: f64 = 30.0 * MIN;

#[derive(Clone, Debug, PartialEq)]
pub enum Move {
    /// `systemctl VERB UNIT`: restart a runner that has stopped taking jobs,
    /// or start one that has failed.
    Runner {
        unit: String,
        verb: &'static str,
        why: String,
    },
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Decision {
    pub moves: Vec<Move>,
    /// The watch file's items, by key: runner/UNIT, deploy/OWNER/REPO and
    /// github/OWNER/REPO.
    pub items: BTreeMap<String, Item>,
    /// When its next pass should look again, for a move of its own.
    pub wake: Option<f64>,
    /// The state to keep, the moves not yet in its log.
    pub state: State,
}

/// The pass's decision, from what it saw (`snap`), what it kept (`st`) and
/// its last watch file's items (`was`), which stand for a repo it couldn't
/// read this time.
pub fn decide(
    cfg: &Config,
    snap: &Snapshot,
    st: &State,
    was: &BTreeMap<String, Item>,
    now: f64,
) -> Decision {
    let mut d = Decision {
        state: State {
            log: st.log.clone(),
            ..State::default()
        },
        ..Decision::default()
    };
    for (repo, err) in &snap.unread {
        let since = *st.unread.get(repo).unwrap_or(&now);
        d.state.unread.insert(repo.clone(), since);
        d.items.insert(
            format!("github/{repo}"),
            Item {
                what: format!("{repo}, as the pipeline dispatcher reads it from GitHub"),
                phase: format!("its passes can't read it: {err}"),
                since,
                due: since + UNREAD,
                link: Some(format!("https://github.com/{repo}")),
                kick: Some(cfg.handler.clone()),
            },
        );
    }
    for (unit, r) in &cfg.runners {
        let key = format!("runner/{unit}");
        let recent: Vec<f64> = st
            .restarts
            .get(unit)
            .map(|v| v.iter().copied().filter(|t| *t > now - RECENT).collect())
            .unwrap_or_default();
        if !recent.is_empty() {
            d.state.restarts.insert(unit.clone(), recent.clone());
        }
        let Some(seen) = snap.runners.get(unit) else {
            // Its repo wasn't read: as the last pass saw it.
            if snap.unread.contains_key(&r.repo) {
                if let Some(it) = was.get(&key) {
                    d.items.insert(key, it.clone());
                }
                if let Some(t) = st.idle.get(unit) {
                    d.state.idle.insert(unit.clone(), *t);
                }
            }
            continue;
        };
        runner(
            cfg,
            snap,
            unit,
            seen,
            &recent,
            st.idle.get(unit).copied(),
            now,
            &mut d,
        );
    }
    if let (Some(dep), Some((update, said))) = (&cfg.deploy, &snap.update) {
        for repo in dep.running.keys() {
            let key = format!("deploy/{repo}");
            let Some(b) = snap.deploys.get(repo) else {
                // Not read: as the last pass saw it.
                if let (true, Some(it)) = (snap.unread.contains_key(repo), was.get(&key)) {
                    d.items.insert(key, it.clone());
                }
                continue;
            };
            if b.count == 0 {
                continue;
            }
            let link = Some(format!(
                "https://github.com/{repo}/compare/{}...{}",
                b.running, b.branch
            ));
            let what = format!("{repo}'s {} going live", b.branch);
            let item = match update.since {
                // A run started after the oldest commit has it.
                Some(start) if update.state == "activating" && start >= b.oldest => Item {
                    what,
                    phase: format!(
                        "{} has been putting it live since {}",
                        dep.unit,
                        clock(start)
                    ),
                    since: start,
                    due: start + dep.run,
                    link,
                    kick: None,
                },
                _ => Item {
                    what,
                    phase: format!(
                        "{} the running system wasn't built from, the oldest from {} (the newest {}); {}'s last run: {said}",
                        commits(b.count),
                        clock(b.oldest),
                        b.newest.get(..7).unwrap_or(&b.newest),
                        dep.unit,
                    ),
                    since: b.oldest,
                    due: b.oldest + dep.after,
                    link,
                    kick: Some(dep.handler.clone()),
                },
            };
            d.items.insert(key, item);
        }
    }
    d
}

/// One runner's wait, and its move: what's queued for it waits while it's
/// at a job, and otherwise it's taking none of it. An active runner idle for
/// `quiet` with work queued has lost GitHub and is restarted; a failed one,
/// started. Neither while a switch is in flight (it restarts what it
/// changes), nor past RESTARTS in RECENT.
#[allow(clippy::too_many_arguments)]
fn runner(
    cfg: &Config,
    snap: &Snapshot,
    unit: &str,
    seen: &Seen,
    recent: &[f64],
    idle: Option<f64>,
    now: f64,
    d: &mut Decision,
) {
    let Some(oldest) = seen.queued.first() else {
        return;
    };
    if seen.unit.busy {
        return;
    }
    let since = idle.unwrap_or(now);
    d.state.idle.insert(unit.to_owned(), since);
    let n = seen.queued.len();
    let queue = format!(
        "{} queued for it (the oldest, {}, since {})",
        jobs(n),
        oldest.name,
        clock(oldest.at)
    );
    let state = seen.unit.state.as_str();
    let phase = match state {
        "active" => format!("{queue}, and it takes none"),
        _ => format!("it's {state}, with {queue}"),
    };
    d.items.insert(
        format!("runner/{unit}"),
        Item {
            what: format!("{unit}, {}'s Actions runner", seen.repo),
            phase,
            since,
            // Its restarts have had their time by then.
            due: since + (RESTARTS as f64 + 1.0) * cfg.quiet,
            link: Some(oldest.url.clone()),
            kick: None,
        },
    );
    if snap.switching || recent.len() >= RESTARTS {
        if snap.switching {
            book(d, now + cfg.quiet);
        }
        return;
    }
    let why = format!("idle since {} with {queue}", clock(since));
    match state {
        "failed" => d.moves.push(Move::Runner {
            unit: unit.to_owned(),
            verb: "start",
            why,
        }),
        "active" => {
            let ready = [Some(since), seen.unit.since, recent.last().copied()]
                .into_iter()
                .flatten()
                .fold(since, f64::max)
                + cfg.quiet;
            if now >= ready {
                d.moves.push(Move::Runner {
                    unit: unit.to_owned(),
                    verb: "restart",
                    why,
                });
            } else {
                book(d, ready);
            }
        }
        // Starting, stopping, or stopped on purpose: a look later.
        _ => book(d, now + cfg.quiet),
    }
}

fn book(d: &mut Decision, at: f64) {
    d.wake = Some(d.wake.map_or(at, |w| w.min(at)));
}

fn jobs(n: usize) -> String {
    if n == 1 {
        "1 job".into()
    } else {
        format!("{n} jobs")
    }
}

fn commits(n: u64) -> String {
    if n == 1 {
        "1 commit".into()
    } else {
        format!("{n} commits")
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use burrow_watchdog::watch::Item;
    use burrow_watchdog::words::clock;

    use super::{Decision, Move, RECENT, decide};
    use crate::snapshot::tests::config;
    use crate::snapshot::{Behind, Job, Seen, Snapshot};
    use crate::state::State;
    use crate::world::Unit;

    const T: f64 = 1_800_000_000.0;
    const Q: f64 = 300.0;

    fn seen(state: &str, since: f64, busy: bool, queued: &[(&str, f64)]) -> Seen {
        Seen {
            repo: "sam/r".into(),
            unit: Unit {
                state: state.into(),
                since: Some(since),
                busy,
            },
            queued: queued
                .iter()
                .map(|(name, at)| Job {
                    name: (*name).into(),
                    url: format!("https://x/{name}"),
                    at: *at,
                })
                .collect(),
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
            .map(|Move::Runner { unit, verb, .. }| (unit.as_str(), *verb))
            .collect()
    }

    // An idle runner with work queued is watched from the pass that first
    // sees it so, restarted once it has been idle `quiet` (and up that long
    // itself), restarted again `quiet` after that, and then left to the
    // watchdog, whose deadline its restarts don't move.
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
        assert_eq!((it.since, it.due, it.kick.clone()), (T, T + 3.0 * Q, None));
        assert_eq!(it.link.as_deref(), Some("https://x/gate"));
        assert_eq!(
            it.phase,
            format!(
                "2 jobs queued for it (the oldest, gate, since {}), and it takes none",
                clock(T - 900.0)
            )
        );
        st = d.state;

        let d = decide(&cfg, &s, &st, &none, T + Q);
        assert_eq!(restarts(&d), [("ci", "restart")]);
        assert_eq!(d.items["runner/ci"].since, T);
        st = d.state;
        st.restarts.insert("ci".into(), vec![T + Q]);

        // Freshly restarted: it gets its own `quiet` before the next.
        let s = snap(seen("active", T + Q, false, &[("gate", T - 900.0)]));
        let d = decide(&cfg, &s, &st, &none, T + Q + 10.0);
        assert!(d.moves.is_empty() && d.wake == Some(T + 2.0 * Q), "{d:?}");
        let d = decide(&cfg, &s, &st, &none, T + 2.0 * Q);
        assert_eq!(restarts(&d), [("ci", "restart")]);
        st = d.state;
        st.restarts.get_mut("ci").unwrap().push(T + 2.0 * Q);

        // Twice in RECENT: no more, and its wait falls due as it was.
        let d = decide(&cfg, &s, &st, &none, T + 3.0 * Q);
        assert!(d.moves.is_empty() && d.wake.is_none(), "{d:?}");
        assert_eq!(d.items["runner/ci"].due, T + 3.0 * Q);
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

    // A failed runner is started at once; one that's stopped, starting or
    // stopping is only watched, and looked at again; none moves while a
    // switch is in flight.
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
        // A run under way that started before the oldest commit doesn't
        // have it; one that started after does.
        s.update.as_mut().unwrap().0 = Unit {
            state: "activating".into(),
            since: Some(T - 1.0),
            busy: false,
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
            (T + 30.0, T + 30.0 + 5400.0, None)
        );
        assert_eq!(
            it.phase,
            format!(
                "box-update has been putting it live since {}",
                clock(T + 30.0)
            )
        );
    }
}
