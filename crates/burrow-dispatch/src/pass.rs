//! A pass: look, decide, make the moves, and say what it waits on.

use std::collections::BTreeMap;
use std::fmt::Write;

use burrow_watchdog::state::{load, save};
use burrow_watchdog::watch::{self, File};
use burrow_watchdog::words::{clock, line};

use crate::config::Config;
use crate::rules::{Decision, Move, decide};
use crate::snapshot::{self, Snapshot};
use crate::state::{Done, State};
use crate::world::World;

/// How many of its moves the log keeps.
pub const LOG: usize = 100;

pub fn run(cfg: &Config, world: &dyn World, now: f64) -> Result<(), String> {
    let dir = &cfg.state_dir;
    let st: State = load(dir, "state.json");
    let was = watch::read(&dir.join("watch.json"))
        .map(|(_, items)| items)
        .unwrap_or_default();
    let snap = snapshot::take(cfg, world);
    for (repo, err) in &snap.unread {
        println!("couldn't read {repo}: {err}");
    }
    let Decision {
        moves,
        items,
        wake,
        mut state,
    } = decide(cfg, &snap, &st, &was, now);
    for m in moves {
        let done = match &m {
            Move::Runner { unit, verb, .. } => {
                let done = world.restart(unit, verb);
                // One systemd refused isn't counted against the runner.
                if done.is_ok() {
                    state.restarts.entry(unit.clone()).or_default().push(now);
                }
                done
            }
            Move::Cancel { repo, run, .. } => {
                state.cancels.insert(format!("{repo}#{run}"), now);
                world.cancel(repo, *run)
            }
        };
        let result = done.map_or_else(|e| e, |()| "ok".to_owned());
        println!("{} ({}): {result}", m.what(), m.why());
        state.log.push(Done {
            at: now,
            what: m.what(),
            why: m.why().to_owned(),
            result,
        });
    }
    let keep = state.log.len().saturating_sub(LOG);
    state.log.drain(..keep);
    save(dir, "state.json", &state)?;
    let changed = items != was;
    save(dir, "watch.json", &File { at: now, items })?;
    // What's new may fall due before the watchdog's booked pass, and what
    // has moved on is no stall.
    if let (true, Some(w)) = (changed, &cfg.watchdog) {
        world.kick(w);
    }
    let wake: BTreeMap<&str, f64> = wake.into_iter().map(|t| ("", t)).collect();
    save(dir, "wake.json", &wake)
}

/// What it sees, what it waits on and what it would do now, with the moves
/// it has made: changing nothing.
pub fn status(cfg: &Config, world: &dyn World, now: f64) -> String {
    let dir = &cfg.state_dir;
    let st: State = load(dir, "state.json");
    let was = watch::read(&dir.join("watch.json"))
        .map(|(_, items)| items)
        .unwrap_or_default();
    let snap = snapshot::take(cfg, world);
    let d = decide(cfg, &snap, &st, &was, now);
    let mut out = String::new();
    seen(&mut out, cfg, &snap);
    for it in d.items.values() {
        let _ = writeln!(
            out,
            "waiting: {}: {}, due {}",
            line(it),
            it.phase,
            clock(it.due)
        );
    }
    for m in &d.moves {
        let _ = writeln!(out, "would {}: {}", m.what(), m.why());
    }
    for done in &st.log {
        let _ = writeln!(
            out,
            "{}: {} ({}): {}",
            clock(done.at),
            done.what,
            done.why,
            done.result
        );
    }
    out.trim_end().to_owned()
}

fn seen(out: &mut String, cfg: &Config, snap: &Snapshot) {
    if snap.switching {
        out.push_str("a switch is in flight\n");
    }
    for (unit, r) in &cfg.runners {
        let Some(s) = snap.runners.get(unit) else {
            let _ = writeln!(out, "{unit} ({}): couldn't read its repo", r.repo);
            continue;
        };
        let since = s
            .unit
            .since
            .map(|t| format!(" since {}", clock(t)))
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "{unit} ({}): {}{since}, {}, {} queued for it",
            r.repo,
            s.unit.state,
            if s.unit.busy { "at a job" } else { "idle" },
            s.queued.len()
        );
    }
    if let (Some((u, said)), Some(d)) = (&snap.update, &cfg.deploy) {
        let _ = writeln!(out, "{}: {}; its last run: {said}", d.unit, u.state);
    }
    for (repo, b) in &snap.deploys {
        let _ = match b.count {
            0 => writeln!(out, "{repo}: live"),
            n => writeln!(
                out,
                "{repo}: {n} on {} not live, the oldest from {}",
                b.branch,
                clock(b.oldest)
            ),
        };
    }
    for (repo, err) in &snap.unread {
        let _ = writeln!(out, "{repo}: couldn't read it: {err}");
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use burrow_watchdog::state::load;
    use burrow_watchdog::watch;

    use super::run;
    use crate::snapshot::tests::{Fake, config, job, push, runs};
    use crate::state::State;
    use crate::world::Unit;

    // A pass end to end: the restart it calls for is made and logged, its
    // waits are written for the watchdog, which is kicked only when they
    // change, and its next look is booked.
    #[test]
    fn pass() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = config(dir.path());
        cfg.deploy = None;
        cfg.runners.retain(|u, _| u == "ci");
        let mut w = repo();
        w.github.insert(
            "repos/sam/r/actions/runs?status=queued&per_page=100".into(),
            runs(&[1]),
        );
        w.github.insert(
            "repos/sam/r/actions/runs/1/jobs?filter=latest&per_page=100".into(),
            Ok(json!({"jobs": [job("gate", "queued", "2026-10-29T17:00:00Z", &["self-hosted", "sam"])]})),
        );
        w.units.insert(
            "ci".into(),
            Unit {
                state: "active".into(),
                since: Some(0.0),
                busy: false,
            },
        );
        let t = 1_793_293_200.0; // 17:00Z

        run(&cfg, &w, t).unwrap();
        assert_eq!(*w.did.borrow(), ["kick watchdog"]);
        let wake: serde_json::Value = load(dir.path(), "wake.json");
        assert_eq!(wake, json!({"": t + 300.0}));
        let (at, items) = watch::read(&dir.path().join("watch.json")).unwrap();
        assert_eq!(
            (at, items.keys().map(String::as_str).collect::<Vec<_>>()),
            (t, vec!["runner/ci"])
        );

        // The same wait: no kick. Its time come: restarted.
        run(&cfg, &w, t + 300.0).unwrap();
        assert_eq!(*w.did.borrow(), ["kick watchdog", "restart ci"]);
        let st: State = load(dir.path(), "state.json");
        assert_eq!(st.restarts["ci"], [t + 300.0]);
        assert_eq!(st.log.len(), 1);
        assert_eq!(
            (st.log[0].what.as_str(), st.log[0].result.as_str()),
            ("restart ci", "ok")
        );

        // One systemd refuses is logged with why, and isn't counted.
        w.fails = Some("Access denied".into());
        w.units.insert(
            "ci".into(),
            Unit {
                state: "active".into(),
                since: Some(t + 300.0),
                busy: false,
            },
        );
        run(&cfg, &w, t + 600.0).unwrap();
        let st: State = load(dir.path(), "state.json");
        assert_eq!(st.restarts["ci"], [t + 300.0]);
        assert_eq!(st.log[1].result, "Access denied");

        // Taken: nothing waits, and the watchdog hears.
        w.github.insert(
            "repos/sam/r/actions/runs?status=queued&per_page=100".into(),
            runs(&[]),
        );
        w.did.borrow_mut().clear();
        run(&cfg, &w, t + 700.0).unwrap();
        assert_eq!(*w.did.borrow(), ["kick watchdog"]);
        assert!(
            watch::read(&dir.path().join("watch.json"))
                .unwrap()
                .1
                .is_empty()
        );
        let wake: serde_json::Value = load(dir.path(), "wake.json");
        assert_eq!(wake, json!({}));
    }

    /// sam/r, on main, with nothing live.
    fn repo() -> Fake {
        let mut w = Fake::default();
        w.github
            .insert("repos/sam/r".into(), Ok(json!({"default_branch": "main"})));
        for status in ["queued", "in_progress", "pending"] {
            w.github.insert(
                format!("repos/sam/r/actions/runs?status={status}&per_page=100"),
                runs(&[]),
            );
        }
        w
    }

    // A moot run is cancelled and logged, and tried once, even when GitHub
    // refuses.
    #[test]
    fn cancel() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = config(dir.path());
        cfg.deploy = None;
        cfg.runners.retain(|u, _| u == "ci");
        let mut w = repo();
        w.github.insert(
            "repos/sam/r/actions/runs?status=queued&per_page=100".into(),
            Ok(json!({"workflow_runs": [push(1, 7, "main", "a1", "queued", "2026-10-29T16:00:00Z")]})),
        );
        w.github.insert(
            "repos/sam/r/actions/runs?status=pending&per_page=100".into(),
            Ok(json!({"workflow_runs": [push(2, 7, "main", "b2", "pending", "2026-10-29T16:10:00Z")]})),
        );
        w.github.insert(
            "repos/sam/r/actions/runs/1/jobs?filter=latest&per_page=100".into(),
            Ok(json!({"jobs": []})),
        );
        w.fails = Some("HTTP 403: Resource not accessible by integration".into());
        let t = 1_793_293_200.0;

        run(&cfg, &w, t).unwrap();
        assert_eq!(*w.did.borrow(), ["cancel sam/r#1"]);
        let st: State = load(dir.path(), "state.json");
        assert_eq!(st.cancels["sam/r#1"], t);
        assert_eq!(
            (st.log[0].what.as_str(), st.log[0].result.as_str()),
            (
                "cancel https://github.com/sam/r/actions/runs/1",
                "HTTP 403: Resource not accessible by integration"
            )
        );

        run(&cfg, &w, t + 60.0).unwrap();
        assert_eq!(*w.did.borrow(), ["cancel sam/r#1"]);
    }
}
