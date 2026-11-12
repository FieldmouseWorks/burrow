//! A pass: each stall a step further up the ladder (kick, job, stuck issue,
//! owner), and no further while that step has time left. And the two
//! commands that read and change what it keeps: snooze and status.

use std::collections::BTreeSet;

use serde_json::json;
use sha2::{Digest, Sha256};

use crate::config::Config;
use crate::state::{Stall, State, load, published, save};
use crate::watch::{overdue, watched};
use crate::words::{clock, day, entry, line, notice as notice_words, task};
use crate::world::World;
use crate::{GRACE, MIN, PROJ, RENOTIFY, RETRY, SNOOZE_MAX, START, STEWARD};

/// A job's states in proj that mean it's at work ("": proj can't say),
/// rather than queued or over.
const LIVE: [&str; 2] = ["running", ""];

pub fn run(cfg: &Config, world: &dyn World, now: f64) -> Result<(), String> {
    let dir = &cfg.state_dir;
    let State {
        mut stalls,
        mut missing,
        mut closing,
        withdraw,
        limit,
    } = load(dir, "state.json");
    // A hold #428's watchdog kept for itself: proj's, machine-wide, now.
    // Kept to hand over again if proj didn't take it.
    let limit = limit.filter(|&t| t > now).filter(|t| {
        let (ok, _) = world.proj(&["usage", "adopt", &format!("{}", t.ceil() as i64)]);
        !ok
    });
    // A state from before it kept these has every job its stalls on the stuck
    // issue were handed still to go.
    let mut withdraw = withdraw.unwrap_or_else(|| {
        stalls
            .values()
            .filter(|st| st.entry.is_some())
            .filter_map(|st| st.job.clone())
            .collect()
    });
    let before = published(&stalls);
    let (found, due) = overdue(cfg, now, &mut missing);
    let mut wakes: Vec<f64> = due.into_iter().collect();

    let gone: Vec<String> = stalls
        .keys()
        .filter(|s| !found.contains_key(*s))
        .cloned()
        .collect();
    for s in gone {
        let st = stalls.remove(&s).expect("a stall it has");
        println!("moving again: {}", line(&st.item));
        withdraw.extend(st.job.clone());
        if st.told.is_some() && !cfg.notify.is_empty() {
            closing.insert(s, st);
        }
    }
    let back: Vec<String> = closing
        .keys()
        .filter(|s| found.contains_key(*s))
        .cloned()
        .collect();
    for s in back {
        let st = closing.remove(&s).expect("a stall it has");
        println!("stalled again, its notice still open: {}", line(&st.item));
        stalls.insert(s, st);
    }
    closing.retain(|s, _| {
        close(cfg, world, s, MOVING);
        held(cfg, world, s)
    });

    // The watchdog's own jobs in proj's line are its own steps' to time:
    // another job for one would queue behind it.
    let mine: BTreeSet<String> = stalls.values().filter_map(|st| st.job.clone()).collect();
    // One proj didn't take holds here till it does.
    let spent = out_of_usage(world, now).or_else(|| {
        limit.map(|t| {
            (
                t,
                format!(
                    "a usage limit an earlier watchdog recorded, until {}",
                    day(t)
                ),
            )
        })
    });
    let mut waiting = 0;
    let mut ready = Vec::new();
    for (s, cur) in found {
        if cur.source == PROJ && mine.contains(&cur.key) {
            continue;
        }
        // Out of usage, every Claude job waits for it (proj's usage.rs), so
        // what only a Claude can move can't move, and a job of the
        // watchdog's own would only wait too. Held, not a stall, until it
        // resets: proj tells the owner of it, once. One already on the stuck
        // issue, or the owner's, stays there. After, it starts the ladder
        // again from its kick. Anything else stalled meanwhile is a failure
        // of its own: it has its kick, and goes to the owner by the deadline
        // a job's would have had, since no Claude can take it (`out`).
        let out = spent.as_ref().map(|(until, _)| *until);
        if let Some(until) = out
            && cur.item.needs_claude()
            && !stalls
                .get(&s)
                .is_some_and(|st| st.entry.is_some() || st.told.is_some())
        {
            if let Some(st) = stalls.remove(&s) {
                withdraw.extend(st.job);
            }
            waiting += 1;
            wakes.push(until);
            continue;
        }
        // Looked at again once usage is back: its next step may be a
        // Claude's again.
        wakes.extend(out);
        let st = stalls.entry(s.clone()).or_insert_with(|| Stall {
            first: now,
            ..Stall::default()
        });
        st.source = cur.source;
        st.key = cur.key;
        st.item = cur.item;
        let kick = st.item.kick.clone().filter(|k| !k.is_empty());
        // On the stuck issue, but one it can't take now (no steward to
        // publish it, or it has turned out to be that issue's own job): off
        // it, whatever else stands, and on through the steps below to the
        // owner, or on with their notice if they've had one.
        if st.entry.is_some() && !issue_takes(cfg, st) {
            st.entry = None;
        }
        if let Some(until) = st.snoozed.filter(|&t| t > now) {
            wakes.push(until);
        } else if let Some(until) = out.filter(|_| st.entry.is_some() && !st.item.needs_claude()) {
            // On the stuck issue, but its job is a Claude too, which can't
            // take it while usage is out: the owner hears meanwhile, and once
            // it's back their notice closes and that job takes it on (below).
            if st.told.is_none_or(|told| now >= told + RENOTIFY) {
                tell(cfg, world, &s, st, now, Some(until));
            }
        } else if st.entry.is_some() {
            // On the stuck issue, whose own job takes it from there: none of
            // it is the owner's. One they were told of before that job took
            // it on has its notice closed, again each pass until it is.
            if st.told.is_some() {
                close(cfg, world, &s, ON_ISSUE);
                if !held(cfg, world, &s) {
                    st.told = None;
                }
            }
        } else if let Some(told) = st.told {
            if now >= told + RENOTIFY {
                tell(cfg, world, &s, st, now, out);
            }
        } else if let Some(job) = st.job.clone() {
            let state = world.proj(&["state", &job]).1;
            let handed = st.handed.unwrap_or(st.first);
            let at = if state == "queued" {
                // What it's about may be the very slot it waits for.
                handed + START.min(cfg.escalate_after)
            } else if LIVE.contains(&state.as_str()) {
                handed + cfg.escalate_after
            } else {
                // Its fix gets time to show: GRACE, or the time it asked for.
                match st.snoozed {
                    Some(until) => until,
                    None => *st.ended.get_or_insert(now) + GRACE,
                }
            };
            if now < at {
                wakes.push(at);
            } else {
                escalate(cfg, world, &s, st, now, out);
                withdraw.insert(job);
            }
        } else if st.item.line {
            // Waiting for a slot: a job would join the same line, ahead of
            // it. The stuck issue's one job looks at the line as a whole.
            escalate(cfg, world, &s, st, now, out);
        } else if let (Some(kick), None) = (&kick, st.kicked) {
            println!("stalled, kicked {kick}: {}", line(&st.item));
            world.kick(kick);
            st.kicked = Some(now);
            wakes.push(now + GRACE);
        } else if let Some(kicked) = st.kicked.filter(|&k| now < k + GRACE) {
            wakes.push(kicked + GRACE);
        } else if (st.tried.is_some() || out.is_some()) && now >= st.first + cfg.escalate_after {
            // No job could take it all that time.
            escalate(cfg, world, &s, st, now, out);
        } else if out.is_some() {
            // Out of usage, a job would only wait in line: none is started,
            // as when none could be.
            wakes.push(st.first + cfg.escalate_after);
        } else if let Some(tried) = st.tried.filter(|&t| now < t + RETRY) {
            wakes.push((tried + RETRY).min(st.first + cfg.escalate_after));
        } else {
            ready.push(s);
        }
    }

    if let Some((_, said)) = spent.filter(|_| waiting > 0) {
        println!("{waiting} held, not stalls: {said}");
    }

    if !ready.is_empty() {
        let jid = start_job(
            cfg,
            world,
            &ready.iter().map(|s| &stalls[s]).collect::<Vec<_>>(),
        );
        for s in &ready {
            let st = stalls.get_mut(s).expect("a stall it has");
            match &jid {
                Some(jid) => {
                    println!("stalled, handed to proj job {jid}: {}", line(&st.item));
                    st.job = Some(jid.clone());
                    st.handed = Some(now);
                    st.tried = None;
                }
                None => {
                    println!("stalled, and no job could start: {}", line(&st.item));
                    st.tried = Some(now);
                }
            }
        }
        // Its next look comes by the time its next step could: a queued
        // job's escalation, or a retry's (escalateAfter may be the shorter).
        wakes.push(match jid {
            Some(_) => now + START.min(cfg.escalate_after),
            None => (now + RETRY).min(
                ready
                    .iter()
                    .map(|s| stalls[s].first)
                    .fold(f64::INFINITY, f64::min)
                    + cfg.escalate_after,
            ),
        });
    }

    // Its own jobs whose stalls have all moved on, moving again or on to the
    // stuck issue's job: one still waiting for a slot has done nothing yet,
    // and would only take one from the line for what's no longer its to fix.
    // One the owner was told of keeps its job, the only Claude still on it.
    let needed: BTreeSet<&String> = stalls
        .values()
        .filter(|st| st.entry.is_none())
        .filter_map(|st| st.job.as_ref())
        .collect();
    withdraw.retain(|job| needed.contains(job) || !withdrawn(world, job));

    // The stuck issue: the steward publishes this with the rest of it, on
    // the pass this kicks when it changed.
    let after = published(&stalls);
    save(dir, "escalated.json", &after)?;
    if after != before
        && let Some(steward) = &cfg.steward_kick
    {
        world.kick(steward);
    }
    save(
        dir,
        "state.json",
        &State {
            stalls,
            missing,
            closing,
            withdraw: Some(withdraw),
            limit,
        },
    )?;
    let wake = match wakes.into_iter().reduce(f64::min) {
        Some(at) => json!({"": at}),
        None => json!({}),
    };
    save(dir, "wake.json", &wake)
}

/// When proj says the machine is out of Claude usage (`proj usage`): when it
/// resets, and proj's line on it. None when it isn't, or proj can't say.
fn out_of_usage(world: &dyn World, now: f64) -> Option<(f64, String)> {
    let (ok, out) = world.proj(&["usage"]);
    let v: serde_json::Value = serde_json::from_str(&out).ok().filter(|_| ok)?;
    let until = v["until"].as_f64().filter(|&t| t > now)?;
    let said = v["said"].as_str().unwrap_or_default();
    Some((
        until,
        format!("Claude is out of usage until {} (\"{said}\")", day(until)),
    ))
}

/// The job's id, when one could start. Its end kicks the next pass, which
/// acts on what the job did (--owned).
fn start_job(cfg: &Config, world: &dyn World, stalls: &[&Stall]) -> Option<String> {
    let (timeout, idle) = (
        format!("{}m", cfg.job_timeout),
        format!("{}m", cfg.job_idle),
    );
    let task = task(cfg, stalls);
    let mut args = vec!["run", "--pipeline"];
    // It's the machine's own work, so it acts on GitHub as the machinery's
    // App, where there is one.
    if let Some(app) = &cfg.app {
        args.extend(["--app", app.as_str()]);
    }
    args.extend([
        "--timeout",
        &timeout,
        "--idle",
        &idle,
        "--kick",
        "watchdog",
        // The kind of work it is, which the machine may give a model of its own.
        "--role",
        "watchdog",
        "--owned",
        &cfg.project,
        &task,
    ]);
    let (ok, out) = world.proj(&args);
    (ok && !out.is_empty()).then_some(out)
}

/// Its own job ended if its turn still waits for a slot (proj withdraw, which
/// looks and stops under the lock a turn takes its slot under, so one that
/// has started is never stopped): whether that's settled, the job withdrawn,
/// at work or over. One proj can't settle is asked about again next pass.
fn withdrawn(world: &dyn World, job: &str) -> bool {
    let (ok, out) = world.proj(&["withdraw", job]);
    if !ok {
        println!("couldn't withdraw proj job {job}, none of whose stalls need it now");
    } else if out == "withdrawn" {
        println!("withdrew proj job {job}: none of its stalls need it now");
    }
    ok
}

/// The step after a job: the steward's stuck issue, whose own job (proj
/// run --unstick) takes it from there, so it's never the owner's; or the
/// owner, when there's no steward to publish that issue, when it's the
/// steward that's stuck, when it's that very job (the issue would only
/// hand it to itself), or while Claude is out of usage (`out`, until when),
/// which that job would only wait out.
fn escalate(cfg: &Config, world: &dyn World, s: &str, st: &mut Stall, now: f64, out: Option<f64>) {
    st.escalated = Some(now);
    if issue_takes(cfg, st) && out.is_none() {
        println!("still stalled, on the stuck issue: {}", line(&st.item));
        st.entry = Some(entry(&st.item));
    } else {
        println!("still stalled, telling {}: {}", cfg.owner, line(&st.item));
        tell(cfg, world, s, st, now, out);
    }
}

/// Whether the stuck issue can take a stall: there's a steward to publish
/// it, and it isn't the steward's own stall, nor that of the job that takes
/// the issue.
fn issue_takes(cfg: &Config, st: &Stall) -> bool {
    cfg.steward_kick.is_some() && !(st.source == STEWARD && st.key.is_empty()) && !st.item.unstick
}

/// The owner's notice, sent (again: notify can't say it went through),
/// saying so when it's theirs because Claude is out of usage (`out`).
fn tell(cfg: &Config, world: &dyn World, s: &str, st: &mut Stall, now: f64, out: Option<f64>) {
    st.told = Some(now);
    if cfg.notify.is_empty() {
        println!("no notify to tell {} with: {}", cfg.owner, line(&st.item));
        return;
    }
    let (title, body) = notice_words(cfg, &st.item, out);
    world.notify(&["open", &notice_key(s), &title, &body]);
}

/// The notice's key in notify: the same for a stall from pass to pass.
pub fn notice_key(s: &str) -> String {
    let hex: String = Sha256::digest(s.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("stalled-{}", &hex[..12])
}

/// What a notice closes with: its stall cleared.
const MOVING: &str = "It's moving again.";
/// What a notice closes with: the stuck issue's job has its stall now.
const ON_ISSUE: &str =
    "It's on the pipeline's stuck issue now, whose own Claude job takes it from there.";

/// The owner's notice of one stall (notify) closed, saying why.
fn close(cfg: &Config, world: &dyn World, s: &str, why: &str) {
    if !cfg.notify.is_empty() {
        world.notify(&["close", &notice_key(s), why]);
    }
}

/// Whether the stall's notice is still open: notify says so (pending), or
/// can't say.
fn held(cfg: &Config, world: &dyn World, s: &str) -> bool {
    !cfg.notify.is_empty() && world.notify(&["pending", &notice_key(s)]) != Some(1)
}

/// More time for the stall of SOURCE's KEY (- for the source itself), once
/// a stall: a job found it still legitimately under way. What it did, or
/// why not.
pub fn snooze(
    cfg: &Config,
    source: &str,
    key: &str,
    minutes: u64,
    why: &str,
    now: f64,
) -> Result<String, String> {
    let key = if key == "-" { "" } else { key };
    let mut saved: State = load(&cfg.state_dir, "state.json");
    let hit: Vec<&mut Stall> = saved
        .stalls
        .values_mut()
        .filter(|st| st.source == source && st.key == key)
        .collect();
    if hit.is_empty() {
        let shown = if key.is_empty() {
            "(the source itself)"
        } else {
            key
        };
        return Err(format!("no stall of {source} {shown}: nothing to snooze"));
    }
    if let Some(st) = hit.iter().find(|st| st.why.is_some()) {
        return Err(format!(
            "{} was given more time once already ({}): it's stuck, not slow",
            line(&st.item),
            st.why.as_deref().unwrap_or_default()
        ));
    }
    let mut said = Vec::new();
    for st in hit {
        let until = now + minutes.min(SNOOZE_MAX) as f64 * MIN;
        st.snoozed = Some(until);
        st.why = Some(why.to_owned());
        said.push(format!(
            "snoozed until {}: {} ({why})",
            clock(until),
            line(&st.item)
        ));
    }
    save(&cfg.state_dir, "state.json", &saved)?;
    Ok(said.join("\n"))
}

/// What each source waits on and how far each stall has gone, changing
/// nothing.
pub fn status(cfg: &Config, now: f64) -> String {
    let saved: State = load(&cfg.state_dir, "state.json");
    let mut out = Vec::new();
    for (name, src) in &cfg.sources {
        let Some((at, items)) = watched(src) else {
            out.push(format!("{name}: no watch file yet"));
            continue;
        };
        out.push(format!(
            "{name}: written {}, {} waiting",
            clock(at),
            items.len()
        ));
        let mut items: Vec<_> = items.into_iter().collect();
        items.sort_by(|a, b| a.1.due.total_cmp(&b.1.due));
        for (key, it) in items {
            out.push(format!(
                "  {}{key}: {} (since {}, due {})",
                if it.due <= now { "OVERDUE " } else { "" },
                it.phase,
                clock(it.since),
                clock(it.due)
            ));
        }
    }
    for st in saved.closing.values() {
        out.push(format!(
            "moving again, its notice still to close: {}",
            line(&st.item)
        ));
    }
    for st in saved.stalls.values() {
        let step = if st.told.is_some() {
            "told the owner".to_owned()
        } else if st.escalated.is_some() {
            "on the stuck issue".to_owned()
        } else if st.snoozed.is_some_and(|t| t > now) {
            format!("more time: {}", st.why.as_deref().unwrap_or_default())
        } else if let Some(job) = &st.job {
            format!("proj job {job}")
        } else if st.kicked.is_some() {
            "kicked".to_owned()
        } else {
            "new".to_owned()
        };
        out.push(format!(
            "stalled ({step}): {}: {}",
            line(&st.item),
            st.item.phase
        ));
    }
    out.join("\n")
}
