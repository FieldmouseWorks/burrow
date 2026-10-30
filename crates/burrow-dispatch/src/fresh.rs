//! The fresh runners' moves and wait: one started for each job queued for
//! them that none idle is there to take, up to `most` at once, and one that
//! has sat idle for `quiet` stopped. A runner registered for one job takes a
//! job queued for its label within seconds of starting; one that hasn't by
//! then never will (the job went to another, or was cancelled), and GitHub
//! drops its registration once it's gone.

use burrow_watchdog::watch::Item;
use burrow_watchdog::words::clock;

use crate::config::{Config, Fresh};
use crate::rules::{Decision, Move, book, jobs};
use crate::snapshot::Pool;
use crate::state::State;

pub fn decide(cfg: &Config, f: &Fresh, pool: &Pool, st: &State, now: f64, d: &mut Decision) {
    // Each unit that's up holds its place: at a job, waiting for one, or
    // on its way in or out.
    let up: Vec<(&String, &crate::world::Unit)> = pool
        .units
        .iter()
        .filter(|(_, u)| {
            matches!(
                u.state.as_str(),
                "active" | "activating" | "deactivating" | "reloading"
            )
        })
        .collect();
    let mut waiting = 0;
    for (instance, u) in &up {
        if u.state != "active" || u.busy {
            continue;
        }
        let since = u.since.unwrap_or(now);
        if now >= since + cfg.quiet {
            d.moves.push(Move::Retire {
                unit: format!("{}{instance}.service", f.template),
                why: format!(
                    "idle since {}, and a runner for one job takes it in seconds",
                    clock(since)
                ),
            });
        } else {
            waiting += 1;
            book(d, since + cfg.quiet);
        }
    }
    let queued = pool.queued.len();
    let room = f.most.saturating_sub(up.len());
    let start = queued.saturating_sub(waiting).min(room);
    let Some(oldest) = pool.queued.first() else {
        return;
    };
    let queue = format!(
        "{} queued for them (the oldest since {})",
        jobs(queued),
        clock(oldest.at)
    );
    for i in 0..start {
        d.moves.push(Move::Fresh {
            instance: format!("{}-{i}", now as u64),
            why: format!("{queue}, {} of {} up, {waiting} idle", up.len(), f.most),
        });
    }
    if start > 0 {
        // Whether it took one: its job starting says so, but not a start
        // that failed.
        book(d, now + cfg.quiet);
    }
    // A queue all `most` of them, at jobs, explain is no stall: the jobs
    // they're at are.
    if room == 0 && waiting == 0 && start == 0 && up.iter().all(|(_, u)| u.busy) {
        return;
    }
    let since = st.fresh.unwrap_or(now);
    d.state.fresh = Some(since);
    d.items.insert(
        "fresh".into(),
        Item {
            what: format!("{}'s runners started fresh for one job each", f.owner),
            phase: format!("{queue}, {} of {} up", up.len(), f.most),
            since,
            due: since + 2.0 * (cfg.quiet + cfg.gap),
            link: Some(oldest.url.clone()),
            kick: Some(cfg.handler.clone()),
            line: false,
        },
    );
}

#[cfg(test)]
mod tests {
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
        let mut d = Decision::default();
        decide(cfg, cfg.fresh.as_ref().unwrap(), p, st, now, &mut d);
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
}
