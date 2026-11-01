//! The fresh runners' moves and wait: one started for each job queued for
//! them that none idle is there to take, and `idle` more kept waiting for
//! jobs to come, up to `most` at once. One idle past those kept is stopped
//! once it has sat `quiet`, and so is one kept that has sat `quiet` beside a
//! job queued for it: a runner waiting for a job takes one queued for its
//! label within seconds. GitHub drops a runner's registration once it's
//! gone, and one kept is still started fresh: nothing ran on it before.

use std::collections::BTreeMap;

use burrow_watchdog::watch::Item;
use burrow_watchdog::words::clock;

use crate::config::{Config, Fresh};
use crate::rules::{Decision, Move, book, jobs};
use crate::snapshot::Pool;
use crate::state::State;

/// `unread`: one of the pool's repos wasn't read this time, so its queue
/// isn't all here. No `pool`: their units couldn't be listed, so how many
/// are up isn't known, and none is started or stopped.
pub fn decide(
    cfg: &Config,
    f: &Fresh,
    pool: Option<&Pool>,
    unread: bool,
    (st, was): (&State, &BTreeMap<String, Item>),
    now: f64,
    d: &mut Decision,
) {
    match pool.and_then(|pool| wait(cfg, f, pool, st, now, d)) {
        Some(item) => {
            d.state.fresh = Some(item.since);
            d.items.insert(KEY.into(), item);
        }
        // What wasn't read may still be waiting: as the last pass saw it.
        None if unread || pool.is_none() => {
            if let Some(it) = was.get(KEY) {
                d.items.insert(KEY.into(), it.clone());
            }
            d.state.fresh = st.fresh;
        }
        None => {}
    }
}

const KEY: &str = "fresh";

/// The moves, and the wait the queue is, if it is one.
fn wait(
    cfg: &Config,
    f: &Fresh,
    pool: &Pool,
    st: &State,
    now: f64,
    d: &mut Decision,
) -> Option<Item> {
    // Each unit that's up holds its place: at a job, waiting for one, or
    // on its way in or out, its start queued behind what it's ordered
    // after among them.
    let up: Vec<(&String, &crate::world::Unit)> = pool
        .units
        .iter()
        .filter(|(_, u)| {
            u.pending
                || matches!(
                    u.state.as_str(),
                    "active" | "activating" | "deactivating" | "reloading"
                )
        })
        .collect();
    // Stopping, so a place a later look can fill.
    let mut leaving = up.iter().filter(|(_, u)| u.state == "deactivating").count();
    // On their way up, so soon waiting for a job.
    let coming = up
        .iter()
        .filter(|(_, u)| !matches!(u.state.as_str(), "active" | "deactivating"))
        .count();
    // Those waiting for a job, newest first: the newest `idle` are kept for
    // jobs to come, however long, and any other is stopped once it has sat
    // `quiet`. So is one that has sat `quiet` beside a job queued for it:
    // it would have taken it within seconds.
    let mut idle: Vec<_> = up
        .iter()
        .filter(|(_, u)| u.state == "active" && !u.busy)
        .collect();
    idle.sort_by(|(_, a), (_, b)| b.since.unwrap_or(now).total_cmp(&a.since.unwrap_or(now)));
    let oldest = pool.queued.first();
    let mut waiting = 0;
    for (instance, u) in idle {
        let since = u.since.unwrap_or(now);
        let missed = oldest.map(|job| job.at.max(since) + cfg.quiet);
        let extra = (waiting >= f.idle).then_some(since + cfg.quiet);
        match missed.into_iter().chain(extra).reduce(f64::min) {
            Some(due) if now >= due => {
                leaving += 1;
                let why = if extra.is_some_and(|e| now >= e) {
                    format!(
                        "idle since {}, and a runner for one job takes it in seconds",
                        clock(since)
                    )
                } else {
                    format!(
                        "idle since {} beside a job queued since {}, which it would have taken in seconds",
                        clock(since),
                        clock(oldest.map_or(since, |job| job.at))
                    )
                };
                d.moves.push(Move::Retire {
                    unit: format!("{}{instance}.service", f.template),
                    why,
                });
            }
            due => {
                waiting += 1;
                if let Some(due) = due {
                    book(d, due);
                }
            }
        }
    }
    let queued = pool.queued.len();
    let room = f.most.saturating_sub(up.len());
    let start = (queued + f.idle).saturating_sub(waiting + coming).min(room);
    let queue = oldest.map(|job| {
        format!(
            "{} queued for them (the oldest since {})",
            jobs(queued),
            clock(job.at)
        )
    });
    let why = format!(
        "{}, {} of {} up, {waiting} idle",
        queue
            .clone()
            .unwrap_or_else(|| format!("{} kept waiting for jobs to come", f.idle)),
        up.len(),
        f.most
    );
    for i in 0..start {
        d.moves.push(Move::Fresh {
            instance: format!("{}-{i}", now as u64),
            why: why.clone(),
        });
    }
    if start + coming > 0 {
        // Whether each came up: a job starting on one says so, but nothing
        // says a start failed, nor that one kept for jobs to come is up.
        // Once they're all up, nothing more is booked for them.
        book(d, now + cfg.quiet);
    }
    // Places a stop is freeing that a job, or one to keep, could take:
    // nothing about a runner stopping wakes a pass, so the next one is
    // booked. Only a stop that's done frees the place, so `most` holds
    // when it isn't.
    if leaving > 0 && queued + f.idle > waiting + coming + start {
        book(d, now);
    }
    // Nothing queued is no wait, whatever is kept for jobs to come.
    let (oldest, queue) = (oldest?, queue?);
    // A queue all `most` of them, at jobs, explain is no stall: the jobs
    // they're at are.
    if room == 0 && waiting == 0 && start == 0 && up.iter().all(|(_, u)| u.busy) {
        return None;
    }
    let since = st.fresh.unwrap_or(now);
    Some(Item {
        what: format!("{}'s runners started fresh for one job each", f.owner),
        phase: format!("{queue}, {} of {} up", up.len(), f.most),
        since,
        due: since + 2.0 * (cfg.quiet + cfg.gap),
        link: Some(oldest.url.clone()),
        kick: Some(cfg.handler.clone()),
        line: false,
        unstick: false,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use burrow_watchdog::watch::Item;

    use super::decide;
    use crate::config::Config;
    use crate::rules::{Decision, Move};
    use crate::snapshot::tests::{config, fresh};
    use crate::snapshot::{Job, Pool};
    use crate::state::State;
    use crate::world::Unit;

    const T: f64 = 1_800_000_000.0;
    const Q: f64 = 300.0;
    const G: f64 = 120.0;

    fn unit(state: &str, since: f64, busy: bool) -> Unit {
        Unit {
            state: state.into(),
            since: Some(since),
            busy,
            pending: false,
        }
    }

    fn pool(units: &[(&str, Unit)], queued: &[f64]) -> Pool {
        Pool {
            units: units
                .iter()
                .map(|(i, u)| ((*i).into(), u.clone()))
                .collect(),
            queued: queued
                .iter()
                .enumerate()
                .map(|(n, at)| Job {
                    url: format!("https://x/job/{n}"),
                    at: *at,
                })
                .collect(),
        }
    }

    fn look(cfg: &Config, p: &Pool, st: &State, now: f64) -> Decision {
        read(cfg, Some(p), false, (st, &BTreeMap::new()), now)
    }

    fn read(
        cfg: &Config,
        p: Option<&Pool>,
        unread: bool,
        last: (&State, &BTreeMap<String, Item>),
        now: f64,
    ) -> Decision {
        let mut d = Decision::default();
        decide(
            cfg,
            cfg.fresh.as_ref().unwrap(),
            p,
            unread,
            last,
            now,
            &mut d,
        );
        d
    }

    /// The instances started and the units stopped.
    fn moves(d: &Decision) -> (Vec<&str>, Vec<&str>) {
        let mut started = Vec::new();
        let mut stopped = Vec::new();
        for m in &d.moves {
            match m {
                Move::Fresh { instance, .. } => started.push(instance.as_str()),
                Move::Retire { unit, .. } => stopped.push(unit.as_str()),
                _ => panic!("{m:?}"),
            }
        }
        (started, stopped)
    }

    fn cfg(dir: &std::path::Path) -> Config {
        let mut cfg = config(dir);
        cfg.fresh = Some(fresh(dir));
        cfg
    }

    // A job queued for them gets a runner of its own, up to `most` at
    // once, and a look `quiet` on; one idle and new is already its.
    #[test]
    fn starts_one_for_each_job_with_room() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        let st = State::default();
        let d = look(&cfg, &pool(&[], &[T - 60.0, T - 30.0, T - 10.0]), &st, T);
        assert_eq!(moves(&d), (vec!["1800000000-0", "1800000000-1"], vec![]));
        assert_eq!(d.wake, Some(T + Q));
        // One up and waiting for its job covers one; one at a job holds a
        // place.
        let p = pool(
            &[
                ("a", unit("active", T - 10.0, false)),
                ("b", unit("active", T - 900.0, true)),
            ],
            &[T - 60.0, T - 30.0],
        );
        let d = look(&cfg, &p, &st, T);
        assert_eq!(moves(&d), (vec![], vec![]));
        assert_eq!(d.wake, Some(T - 10.0 + Q));
        // One up and waiting for its job, with room for another: that's
        // the queue's.
        let p = pool(&[("a", unit("active", T - 10.0, false))], &[T - 60.0]);
        assert_eq!(moves(&look(&cfg, &p, &st, T)), (vec![], vec![]));
        let p = pool(
            &[("a", unit("activating", T - 1.0, false))],
            &[T - 60.0, T - 30.0],
        );
        assert_eq!(moves(&look(&cfg, &p, &st, T)).0, ["1800000000-0"]);
        // Nothing queued: nothing started, nothing watched.
        let d = look(&cfg, &pool(&[], &[]), &st, T);
        assert!(d.moves.is_empty() && d.items.is_empty() && d.state.fresh.is_none());
    }

    // One idle for `quiet` won't take a job, and is stopped, and no longer
    // covers one; one stopping or failed holds no place it could use.
    #[test]
    fn stops_one_left_idle() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        let st = State::default();
        let p = pool(&[("a", unit("active", T - Q, false))], &[]);
        assert_eq!(
            moves(&look(&cfg, &p, &st, T)),
            (vec![], vec!["fresh-runner@a.service"])
        );
        let p = pool(&[("a", unit("active", T - Q, false))], &[T - 60.0]);
        let d = look(&cfg, &p, &st, T);
        assert_eq!(
            moves(&d),
            (vec!["1800000000-0"], vec!["fresh-runner@a.service"])
        );
        let p = pool(
            &[
                ("a", unit("failed", T - 900.0, false)),
                ("b", unit("inactive", T, false)),
            ],
            &[T - 60.0, T - 30.0],
        );
        assert_eq!(moves(&look(&cfg, &p, &st, T)).0.len(), 2);
        // One whose start is queued, behind what it's ordered after, holds
        // its place though it isn't up yet.
        let queued = Unit {
            pending: true,
            ..unit("inactive", T, false)
        };
        let p = pool(&[("b", queued.clone())], &[T - 60.0, T - 30.0]);
        assert_eq!(moves(&look(&cfg, &p, &st, T)).0.len(), 1);
        let p = pool(
            &[("a", queued.clone()), ("b", queued)],
            &[T - 60.0, T - 30.0],
        );
        assert_eq!(moves(&look(&cfg, &p, &st, T)), (vec![], vec![]));
    }

    // Jobs waiting with room for more are a wait for the watchdog, from the
    // pass that first saw them so, due once a start and its look have had
    // their time twice; a queue `most` runners at jobs explain is none.
    #[test]
    fn a_queue_with_room_is_watched() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        let mut st = State::default();
        let p = pool(&[], &[T - 600.0]);
        let d = look(&cfg, &p, &st, T);
        let it = &d.items["fresh"];
        assert_eq!(
            (it.since, it.due, it.kick.as_deref(), it.link.as_deref()),
            (
                T,
                T + 2.0 * (Q + G),
                Some("dispatch"),
                Some("https://x/job/0")
            )
        );
        assert!(
            it.phase.starts_with("1 job queued for them"),
            "{}",
            it.phase
        );
        st = d.state;
        let d = look(&cfg, &p, &st, T + Q);
        assert_eq!(d.items["fresh"].since, T);
        // All of them at jobs: the jobs are what's slow.
        let busy = pool(
            &[
                ("a", unit("active", T, true)),
                ("b", unit("active", T, true)),
            ],
            &[T - 600.0],
        );
        let d = look(&cfg, &busy, &st, T + Q);
        assert!(d.items.is_empty() && d.state.fresh.is_none() && d.moves.is_empty());
        // Room, but the one up isn't at a job: still watched.
        let p = pool(&[("a", unit("active", T + Q - 1.0, false))], &[T - 600.0]);
        assert!(look(&cfg, &p, &st, T + Q).items.contains_key("fresh"));
    }

    // Places a stop frees go to the jobs still queued at the next pass,
    // booked at once since nothing about a stop wakes one: for a pool all
    // left idle, and for one still stopping. Until then `most` holds.
    #[test]
    fn a_place_freed_is_looked_at_again() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        let st = State::default();
        let idle = pool(
            &[
                ("a", unit("active", T - Q, false)),
                ("b", unit("active", T - 2.0 * Q, false)),
            ],
            &[T - 60.0],
        );
        let d = look(&cfg, &idle, &st, T);
        assert_eq!(
            moves(&d),
            (
                vec![],
                vec!["fresh-runner@a.service", "fresh-runner@b.service"]
            )
        );
        assert_eq!(d.wake, Some(T));
        let stopping = pool(
            &[
                ("a", unit("deactivating", T, false)),
                ("b", unit("active", T - 10.0, true)),
            ],
            &[T - 60.0],
        );
        let d = look(&cfg, &stopping, &st, T);
        assert_eq!(moves(&d), (vec![], vec![]));
        assert_eq!(d.wake, Some(T));
        // Nothing queued, or nothing the places would go to: nothing booked.
        let p = pool(&[("a", unit("active", T - Q, false))], &[]);
        assert_eq!(look(&cfg, &p, &st, T).wake, None);
        let p = pool(
            &[
                ("a", unit("deactivating", T, false)),
                ("b", unit("active", T - 10.0, false)),
            ],
            &[T - 60.0],
        );
        assert_eq!(look(&cfg, &p, &st, T).wake, Some(T - 10.0 + Q));
    }

    fn keeping(dir: &std::path::Path, idle: usize, most: usize) -> Config {
        let mut cfg = cfg(dir);
        let f = cfg.fresh.as_mut().unwrap();
        (f.idle, f.most) = (idle, most);
        cfg
    }

    // With `idle`, that many are started before any job asks, looked at
    // again `quiet` on to see they came up, and then kept waiting however
    // long: no wake-up and no wait for them. Only one past them is stopped
    // once it's sat `quiet`, the oldest first.
    #[test]
    fn keeps_idle_ones_for_jobs_to_come() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = keeping(dir.path(), 2, 3);
        let st = State::default();
        let d = look(&cfg, &pool(&[], &[]), &st, T);
        assert_eq!(moves(&d), (vec!["1800000000-0", "1800000000-1"], vec![]));
        assert_eq!(d.wake, Some(T + Q));
        assert!(d.items.is_empty() && d.state.fresh.is_none(), "{d:?}");
        // One that failed to start is started again, and looked at again;
        // one still on its way up is looked at again too.
        let p = pool(
            &[
                ("a", unit("failed", T - Q, false)),
                ("b", unit("activating", T - Q, false)),
            ],
            &[],
        );
        let d = look(&cfg, &p, &st, T);
        assert_eq!(moves(&d), (vec!["1800000000-0"], vec![]));
        assert_eq!(d.wake, Some(T + Q));
        let p = pool(
            &[
                ("a", unit("active", T - 10.0, false)),
                ("b", unit("activating", T - Q, false)),
            ],
            &[],
        );
        let d = look(&cfg, &p, &st, T);
        assert_eq!(moves(&d), (vec![], vec![]));
        assert_eq!(d.wake, Some(T + Q));
        let kept = pool(
            &[
                ("a", unit("active", T - 10.0 * Q, false)),
                ("b", unit("active", T - 20.0 * Q, false)),
            ],
            &[],
        );
        let d = look(&cfg, &kept, &st, T);
        assert_eq!(moves(&d), (vec![], vec![]));
        assert!(d.wake.is_none() && d.items.is_empty(), "{d:?}");
        let three = pool(
            &[
                ("a", unit("active", T - 10.0 * Q, false)),
                ("b", unit("active", T - 20.0 * Q, false)),
                ("c", unit("active", T - 5.0 * Q, false)),
            ],
            &[],
        );
        assert_eq!(
            moves(&look(&cfg, &three, &st, T)),
            (vec![], vec!["fresh-runner@b.service"])
        );
        // One past them, not yet `quiet`, is looked at again then.
        let new = pool(
            &[
                ("a", unit("active", T - 10.0, false)),
                ("b", unit("active", T - 20.0, false)),
                ("c", unit("active", T - 30.0, false)),
            ],
            &[],
        );
        let d = look(&cfg, &new, &st, T);
        assert_eq!(moves(&d), (vec![], vec![]));
        assert_eq!(d.wake, Some(T - 30.0 + Q));
    }

    // One at a job, or on its way up, isn't waiting for one to come: the
    // first is made up for, the second already counts.
    #[test]
    fn only_ones_waiting_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = keeping(dir.path(), 2, 3);
        let st = State::default();
        let p = pool(
            &[
                ("a", unit("active", T - Q, true)),
                ("b", unit("active", T - Q, false)),
            ],
            &[],
        );
        assert_eq!(moves(&look(&cfg, &p, &st, T)).0, ["1800000000-0"]);
        let p = pool(
            &[
                ("a", unit("activating", T, false)),
                ("b", unit("active", T - Q, false)),
            ],
            &[],
        );
        assert_eq!(moves(&look(&cfg, &p, &st, T)), (vec![], vec![]));
        // A queued job counts too: two kept and one for it, room allowing.
        let p = pool(&[("b", unit("active", T - Q, false))], &[T - 5.0]);
        assert_eq!(
            moves(&look(&cfg, &p, &st, T)).0,
            ["1800000000-0", "1800000000-1"]
        );
    }

    // One kept that sits `quiet` beside a job queued for it, without taking
    // it, won't: it's stopped and another started. Until then it's looked
    // at again when it would have.
    #[test]
    fn one_kept_that_leaves_a_job_waiting_is_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = keeping(dir.path(), 1, 2);
        let st = State::default();
        let p = pool(&[("a", unit("active", T - 10.0 * Q, false))], &[T - Q]);
        let d = look(&cfg, &p, &st, T);
        assert_eq!(
            moves(&d),
            (vec!["1800000000-0"], vec!["fresh-runner@a.service"])
        );
        let p = pool(&[("a", unit("active", T - 10.0 * Q, false))], &[T - 10.0]);
        let d = look(&cfg, &p, &st, T);
        assert_eq!(moves(&d), (vec!["1800000000-0"], vec![]));
        assert_eq!(d.wake, Some(T - 10.0 + Q));
    }

    // A pass that couldn't read one of the pool's repos hasn't seen its
    // queue end: the wait stays as the last pass saw it, its deadline too.
    #[test]
    fn a_wait_outlives_an_unread_repo() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        let first = look(&cfg, &pool(&[], &[T - 600.0]), &State::default(), T);
        let (st, was) = (&first.state, &first.items);
        let d = read(&cfg, Some(&pool(&[], &[])), true, (st, was), T + Q);
        assert_eq!((&d.items, d.state.fresh), (was, Some(T)));
        // What it could read still moves, and the wait keeps its age.
        let d = read(&cfg, Some(&pool(&[], &[T - 60.0])), true, (st, was), T + Q);
        assert_eq!(moves(&d).0.len(), 1);
        assert_eq!((d.items["fresh"].since, d.state.fresh), (T, Some(T)));
        // Read, and nothing's queued: it's over.
        let d = read(&cfg, Some(&pool(&[], &[])), false, (st, was), T + Q);
        assert!(d.items.is_empty() && d.state.fresh.is_none());
    }

    // Their units unlisted, how many are up isn't known: none is started
    // or stopped, and the wait stays as the last pass saw it.
    #[test]
    fn nothing_moves_with_the_units_unlisted() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        let first = look(&cfg, &pool(&[], &[T - 600.0]), &State::default(), T);
        let (st, was) = (&first.state, &first.items);
        let d = read(&cfg, None, false, (st, was), T + Q);
        assert!(d.moves.is_empty() && d.wake.is_none(), "{d:?}");
        assert_eq!((&d.items, d.state.fresh), (was, Some(T)));
    }
}
