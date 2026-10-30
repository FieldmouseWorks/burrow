//! What a pass makes of what it saw: the moves its rules call for, and what
//! it waits on, for the stall watchdog. Nothing here reads or acts.

use std::collections::BTreeMap;

use burrow_watchdog::MIN;
use burrow_watchdog::watch::Item;
use burrow_watchdog::words::clock;

use crate::config::Config;
use crate::snapshot::{Run, Seen, Snapshot};
use crate::state::State;

/// Restarts of one runner, and tries at cancelling one run, are counted
/// over this long...
pub const RECENT: f64 = 6.0 * 60.0 * MIN;
/// ...and it gets at most this many: past them, restarting isn't the fix,
/// and its wait goes on to the watchdog.
pub const RESTARTS: usize = 2;
/// A repo GitHub can't be read for this long is a stall of its own.
pub const UNREAD: f64 = 30.0 * MIN;

#[derive(Clone, Debug, PartialEq)]
pub enum Move {
    /// `systemctl VERB UNIT`: restart a runner that has stopped taking jobs,
    /// or start one that has failed. `state` is the one it was seen in.
    Runner {
        unit: String,
        verb: &'static str,
        state: String,
        why: String,
    },
    /// Cancel a push run that hasn't started, that a later one of its
    /// workflow on the same branch has made moot.
    Cancel {
        repo: String,
        run: u64,
        url: String,
        why: String,
    },
    /// Register a runner for one job, and start its unit as this instance.
    Fresh { instance: String, why: String },
    /// Stop a fresh runner's unit that has sat idle: one that hasn't taken
    /// a job in `quiet` won't.
    Retire { unit: String, why: String },
}

impl Move {
    /// The move in a few words, for its log.
    pub fn what(&self) -> String {
        match self {
            Move::Runner { unit, verb, .. } => format!("{verb} {unit}"),
            Move::Cancel { url, .. } => format!("cancel {url}"),
            Move::Fresh { instance, .. } => format!("start fresh runner {instance}"),
            Move::Retire { unit, .. } => format!("stop {unit}"),
        }
    }

    pub fn why(&self) -> &str {
        match self {
            Move::Runner { why, .. }
            | Move::Cancel { why, .. }
            | Move::Fresh { why, .. }
            | Move::Retire { why, .. } => why,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Decision {
    pub moves: Vec<Move>,
    /// The watch file's items, by key: runner/UNIT, fresh, deploy/OWNER/REPO
    /// and github/OWNER/REPO.
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
                line: false,
            },
        );
    }
    for (repo, runs) in &snap.pushes {
        moot(repo, runs, st, now, &mut d);
    }
    // A repo not read this time keeps its runs' tries.
    for (run, at) in &st.cancels {
        let repo = run.rsplit_once('#').map_or("", |(repo, _)| repo);
        if *at > now - RECENT && !snap.pushes.contains_key(repo) {
            d.state.cancels.insert(run.clone(), *at);
        }
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
    if let (Some(f), Some(pool)) = (&cfg.fresh, &snap.fresh) {
        let unread = f.repos.iter().any(|r| snap.unread.contains_key(r));
        crate::fresh::decide(cfg, f, pool, unread, (st, was), now, &mut d);
    }
    if let (Some(dep), Some((update, said))) = (&cfg.deploy, &snap.update) {
        for repo in dep.own.iter().chain(dep.running.keys()) {
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
                    line: false,
                },
                _ => Item {
                    what,
                    phase: format!(
                        "{} the running system wasn't built from, the oldest from {} (the newest {}); {}'s last run: {said}",
                        commits(b.count),
                        clock(b.oldest),
                        short(&b.newest),
                        dep.unit,
                    ),
                    since: b.oldest,
                    due: b.oldest + dep.after,
                    link,
                    kick: Some(dep.handler.clone()),
                    line: false,
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
/// changes), nor past RESTARTS in RECENT. Its wait falls due once each
/// restart, and the look after it, has had its `quiet` and the handler's
/// `gap`; the watchdog then kicks a pass for a fresh look before it goes
/// further.
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
        "{} queued for it (the oldest since {})",
        jobs(n),
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
            due: since + (RESTARTS as f64 + 1.0) * (cfg.quiet + cfg.gap),
            link: Some(oldest.url.clone()),
            kick: Some(cfg.handler.clone()),
            line: false,
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
        "failed" => {
            d.moves.push(Move::Runner {
                unit: unit.to_owned(),
                verb: "start",
                state: state.to_owned(),
                why,
            });
            // Whether it helped: no event says so.
            book(d, now + cfg.quiet);
        }
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
                    state: state.to_owned(),
                    why,
                });
                book(d, now + cfg.quiet);
            } else {
                book(d, ready);
            }
        }
        // Starting, stopping, or stopped on purpose: a look later.
        _ => book(d, now + cfg.quiet),
    }
}

/// A push run on the default branch that no job of has started, and that a
/// later one of its workflow on that branch (queued, next in line, or under
/// way) checks past, is moot: it's cancelled, so the later one gets its
/// place. One that has started is left to finish, so the branch always gets
/// a result, and so is one someone ran again. Each run is tried once in
/// RECENT, whatever came of it.
fn moot(repo: &str, runs: &[Run], st: &State, now: f64, d: &mut Decision) {
    for r in runs
        .iter()
        .filter(|r| r.status == "queued" && r.attempt == 1)
    {
        let Some(later) = runs
            .iter()
            .find(|l| l.id != r.id && l.workflow == r.workflow && l.at > r.at)
        else {
            continue;
        };
        let key = format!("{repo}#{}", r.id);
        if let Some(at) = st.cancels.get(&key).filter(|&&at| at > now - RECENT) {
            d.state.cancels.insert(key, *at);
            continue;
        }
        d.moves.push(Move::Cancel {
            repo: repo.to_owned(),
            run: r.id,
            url: r.url.clone(),
            why: format!(
                "it checks {} on {} and hasn't started, and {} checks {}, which came after it",
                short(&r.sha),
                r.branch,
                later.url,
                short(&later.sha)
            ),
        });
    }
}

fn short(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

pub(crate) fn book(d: &mut Decision, at: f64) {
    d.wake = Some(d.wake.map_or(at, |w| w.min(at)));
}

pub(crate) fn jobs(n: usize) -> String {
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
            },
            queued: queued
                .iter()
                .map(|(name, at)| Job {
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
            fresh: Some(Pool::default()),
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
}
