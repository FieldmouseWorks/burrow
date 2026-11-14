//! What a pass makes of what it saw: the moves its rules call for, and what
//! it waits on, for the stall watchdog. Nothing here reads or acts.

use std::collections::BTreeMap;

use burrow_watchdog::MIN;
use burrow_watchdog::watch::Item;
use burrow_watchdog::words::clock;

use crate::config::{Config, Deploy};
use crate::registration::{Registered, Registration};
use crate::snapshot::{Run, Seen, Snapshot};
use crate::state::State;
use crate::world::Unit;

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
    PrivateFresh {
        repo: String,
        instance: String,
        why: String,
    },
    /// Stop a fresh runner's unit that has sat idle: one that hasn't taken
    /// a job in `quiet` won't.
    Retire { unit: String, why: String },
    /// `systemctl stop` each of `stop`, in turn: the units that start a
    /// runner GitHub has deleted the registration of, then the runner, which
    /// exits as it starts and is started again, without end.
    Halt {
        unit: String,
        stop: Vec<String>,
        why: String,
    },
}

impl Move {
    /// The move in a few words, for its log.
    pub fn what(&self) -> String {
        match self {
            Move::Runner { unit, verb, .. } => format!("{verb} {unit}"),
            Move::Cancel { url, .. } => format!("cancel {url}"),
            Move::Fresh { instance, .. } | Move::PrivateFresh { instance, .. } => {
                format!("start fresh runner {instance}")
            }
            Move::Retire { unit, .. } => format!("stop {unit}"),
            Move::Halt { stop, .. } => format!("stop {}", stop.join(" and ")),
        }
    }

    pub fn why(&self) -> &str {
        match self {
            Move::Runner { why, .. }
            | Move::Cancel { why, .. }
            | Move::Fresh { why, .. }
            | Move::PrivateFresh { why, .. }
            | Move::Retire { why, .. }
            | Move::Halt { why, .. } => why,
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
                repo: Some(repo.clone()),
                what: format!("{repo}, as the pipeline dispatcher reads it from GitHub"),
                phase: format!("its passes can't read it: {err}"),
                since,
                due: since + UNREAD,
                link: Some(format!("https://github.com/{repo}")),
                kick: Some(cfg.handler.clone()),
                line: false,
                unstick: false,
                claude: false,
                refresh: false,
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
                if let Some(t) = st.gone.get(unit) {
                    d.state.gone.insert(unit.clone(), *t);
                }
            }
            continue;
        };
        match &seen.registered {
            Registered::Gone(reg) => {
                let since = st.gone.get(unit).copied();
                gone(cfg, snap.switching, unit, seen, reg, since, now, &mut d);
                continue;
            }
            // Gone when last looked at, and not known now: as the last pass
            // saw it, never restarted on a look that couldn't tell.
            Registered::Unknown(_) if st.gone.contains_key(unit) => {
                d.state.gone.insert(unit.clone(), st.gone[unit]);
                if let Some(it) = was.get(&key) {
                    d.items.insert(key, it.clone());
                }
                book(&mut d, now + cfg.quiet);
                continue;
            }
            _ => {}
        }
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
    crate::private::decide(cfg, snap, st, was, now, &mut d);
    if let (Some(f), Some(pool)) = (&cfg.fresh, &snap.fresh) {
        let unread = f.repos.iter().any(|r| snap.unread.contains_key(r));
        let mut limited = f.clone();
        let private_starts = d
            .moves
            .iter()
            .filter(|m| matches!(m, Move::PrivateFresh { .. }))
            .count();
        limited.most = limited.most.saturating_sub(private_starts);
        if snap
            .private
            .iter()
            .any(|(_, p)| p.as_ref().is_ok_and(|p| !p.queued.is_empty()))
        {
            limited.idle = 0;
        }
        crate::fresh::decide(
            cfg,
            &limited,
            pool.as_ref().ok(),
            unread,
            (st, was),
            now,
            &mut d,
        );
    }
    // A stop emits no workflow event. Keep looking until shared capacity
    // is actually free, even when the retiring runner's own queue is empty.
    if let (Some(f), Some(Ok(pool))) = (&cfg.fresh, &snap.fresh) {
        let demand = !pool.queued.is_empty()
            || f.idle > 0
            || snap
                .private
                .iter()
                .any(|(_, p)| p.as_ref().is_ok_and(|p| !p.queued.is_empty()));
        let retiring = d.moves.iter().any(|m| matches!(m, Move::Retire { .. }))
            || pool.units.values().any(|u| u.state == "deactivating");
        if demand && retiring {
            book(&mut d, now);
        }
    }
    if let (Some(dep), Some((update, said))) = (&cfg.deploy, &snap.update) {
        // Its last attempt failed without saying so (killed, timed out, a
        // command's error): what the run before it said no longer holds.
        let behind = snap.behind.filter(|_| update.state != "failed");
        for repo in dep.own.iter().chain(dep.running.keys()) {
            let key = format!("deploy/{repo}");
            if let Some(why) = snap.unknown.get(repo) {
                // Not established for the system that runs: a wait as old as
                // the last pass's (commits behind, or unknown then too), so
                // a switch or a lost record never drops one, or from now.
                // Its age is kept apart from any run's: only a run that finds
                // main live in the system that runs settles it, never one
                // under way, failed or held.
                let since = st.going_live.get(repo).copied().unwrap_or(now);
                d.state.going_live.insert(repo.clone(), since);
                let what = format!("{repo} going live");
                let link = Some(format!("https://github.com/{repo}"));
                let it = going_live(
                    dep,
                    update,
                    said,
                    snap.progress,
                    behind,
                    what,
                    why,
                    since,
                    link,
                    true,
                    &cfg.handler,
                );
                d.items.insert(key, it);
                continue;
            }
            let Some(b) = snap.deploys.get(repo) else {
                // Not read: as the last pass saw it, but a run's progress is
                // local, so it is taken from what the unit and its logs say
                // now, whatever GitHub or the live record would not.
                if snap.unread.contains_key(repo) {
                    let since = st.going_live.get(repo).copied();
                    if let Some(it) = was.get(&key) {
                        let it = retained(
                            dep,
                            update,
                            said,
                            snap.progress,
                            behind,
                            it,
                            since,
                            now,
                            cfg,
                        );
                        d.items.insert(key, it);
                    }
                    if let Some(t) = since {
                        d.state.going_live.insert(repo.clone(), t);
                    }
                }
                continue;
            };
            if b.count == 0 {
                continue;
            }
            d.state.going_live.insert(repo.clone(), b.oldest);
            let link = Some(format!(
                "https://github.com/{repo}/compare/{}...{}",
                b.running, b.branch
            ));
            let what = format!("{repo}'s {} going live", b.branch);
            let waiting = format!(
                "{} the running system wasn't built from, the oldest from {} (the newest {})",
                commits(b.count),
                clock(b.oldest),
                short(&b.newest),
            );
            let it = going_live(
                dep,
                update,
                said,
                snap.progress,
                behind,
                what,
                &waiting,
                b.oldest,
                link,
                false,
                &cfg.handler,
            );
            d.items.insert(key, it);
        }
    }
    d
}

/// A repo's wait to go live, `waiting` since `since`: on HOST-update's run
/// once one under way has it (one that started since, or, with `any`, any
/// one), due `run` after it last wrote to its logs (`progress`), or after it
/// started if it hasn't since: a run at work, however long its build, isn't
/// stalled, and one that has gone quiet is; the watchdog kicks `refresh`
/// (this handler) first, since a log write triggers no pass and the
/// deadline it was given may be stale. The item says `refresh`, so the
/// watchdog takes it no further than that kick until a pass has written the
/// file again, however long the kick's pass takes to start (it can start
/// only a handler's gap after the last): a run still quiet after it is
/// stalled. Otherwise on HOST-update, which
/// the watchdog kicks: soon, unless
/// its last run is `behind` its own open PR, which brings the commit in only
/// after it lands and has a wait of its own with the steward.
#[allow(clippy::too_many_arguments)]
fn going_live(
    dep: &Deploy,
    update: &Unit,
    said: &str,
    progress: Option<f64>,
    behind: Option<u64>,
    what: String,
    waiting: &str,
    since: f64,
    link: Option<String>,
    any: bool,
    refresh: &str,
) -> Item {
    match update.since {
        Some(start) if update.state == "activating" && (any || start >= since) => Item {
            repo: None,
            what,
            phase: match progress.filter(|p| *p > start) {
                Some(p) => format!(
                    "{} has been putting it live since {}, its last output at {}",
                    dep.unit,
                    clock(start),
                    clock(p)
                ),
                None => format!(
                    "{} has been putting it live since {}",
                    dep.unit,
                    clock(start)
                ),
            },
            since: start,
            due: progress.filter(|p| *p > start).unwrap_or(start) + dep.run,
            link,
            kick: Some(refresh.to_owned()),
            line: false,
            unstick: false,
            claude: false,
            refresh: true,
        },
        _ => Item {
            repo: None,
            what,
            phase: match behind {
                Some(pr) => format!(
                    "{waiting}; behind {}'s own PR #{pr}, until that lands; its last run: {said}",
                    dep.unit
                ),
                None => format!("{waiting}; {}'s last run: {said}", dep.unit),
            },
            since,
            due: since + behind.map_or(dep.after, |_| dep.ride),
            link,
            kick: Some(dep.handler.clone()),
            line: false,
            unstick: false,
            claude: false,
            refresh: false,
        },
    }
}

/// A wait the pass couldn't read (GitHub or the live record failed), as the
/// last pass `was` left it: unchanged while it's waiting on HOST-update, but
/// a run's progress is local, so one under way (or since ended) is taken
/// from the unit and logs now, with the wait's age, text and link kept.
#[allow(clippy::too_many_arguments)]
fn retained(
    dep: &Deploy,
    update: &Unit,
    said: &str,
    progress: Option<f64>,
    behind: Option<u64>,
    was: &Item,
    since: Option<f64>,
    now: f64,
    cfg: &Config,
) -> Item {
    let was_run = was.kick.as_deref() == Some(cfg.handler.as_str());
    // Its run is the one it waited on, or a later one: any run at work that
    // started no earlier than the wait, or than the run the wait was on.
    let ours = update
        .since
        .filter(|_| update.state == "activating")
        .is_some_and(|start| start >= was.since);
    if !ours && !was_run {
        return was.clone();
    }
    going_live(
        dep,
        update,
        said,
        progress,
        behind,
        was.what.clone(),
        if was_run {
            "its commits aren't live yet"
        } else {
            &was.phase
        },
        if was_run {
            since.unwrap_or(now)
        } else {
            was.since
        },
        was.link.clone(),
        was_run,
        &cfg.handler,
    )
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
            repo: Some(seen.repo.clone()),
            what: format!("{unit}, {}'s Actions runner", seen.repo),
            phase,
            since,
            due: since + (RESTARTS as f64 + 1.0) * (cfg.quiet + cfg.gap),
            link: Some(oldest.url.clone()),
            kick: Some(cfg.handler.clone()),
            line: false,
            unstick: false,
            claude: false,
            refresh: false,
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

/// A runner whose registration GitHub has deleted: it takes no job until
/// it's registered again, which no restart does, so it's never restarted,
/// and its wait says so whether or not jobs are queued for it, due once a
/// fresh look has had its time. Started, it exits at once, and whatever
/// starts it again (a path unit on its `.runner`) loops it, so that and the
/// runner are stopped; not while a switch is in flight, which starts what
/// it changes itself.
#[allow(clippy::too_many_arguments)]
fn gone(
    cfg: &Config,
    switching: bool,
    unit: &str,
    seen: &Seen,
    reg: &Registration,
    since: Option<f64>,
    now: f64,
    d: &mut Decision,
) {
    let since = since.unwrap_or(now);
    d.state.gone.insert(unit.to_owned(), since);
    let missing = format!("{} lists no runner {} ({})", reg.scope, reg.id, reg.name);
    let mut phase = format!(
        "its registration is gone at GitHub: {missing}, so it takes no job until it's registered again"
    );
    if let Some(oldest) = seen.queued.first() {
        phase.push_str(&format!(
            "; {} queued for it (the oldest since {})",
            jobs(seen.queued.len()),
            clock(oldest.at)
        ));
    }
    d.items.insert(
        format!("runner/{unit}"),
        Item {
            repo: Some(seen.repo.clone()),
            what: format!("{unit}, {}'s Actions runner", seen.repo),
            phase,
            since,
            due: since + cfg.quiet + cfg.gap,
            link: Some(reg.page()),
            kick: Some(cfg.handler.clone()),
            line: false,
            unstick: false,
            claude: false,
            refresh: false,
        },
    );
    let stop: Vec<String> = seen
        .triggers
        .iter()
        .filter(|(_, u)| up(&u.state))
        .map(|(t, _)| t.clone())
        .chain(up(&seen.unit.state).then(|| unit.to_owned()))
        .collect();
    if stop.is_empty() {
        return;
    }
    if switching {
        book(d, now + cfg.quiet);
        return;
    }
    d.moves.push(Move::Halt {
        unit: unit.to_owned(),
        stop,
        why: format!("its registration is gone at GitHub: {missing}"),
    });
}

/// Whether a unit in this state is running, or starting.
fn up(state: &str) -> bool {
    matches!(state, "active" | "activating" | "reloading" | "refreshing")
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

pub(crate) fn short(sha: &str) -> &str {
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
#[path = "rules_tests.rs"]
mod tests;
