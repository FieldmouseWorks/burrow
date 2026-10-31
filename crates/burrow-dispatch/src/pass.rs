//! A pass: look, decide, make the moves, and say what it waits on.

use std::collections::BTreeMap;
use std::fmt::Write;

use serde_json::Value;

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
        let result = match still(world, &m) {
            Err(why) => format!("not made: {why}"),
            Ok(()) => {
                // Kept before it's made, whatever comes of it, so a pass cut
                // short (its timeout, a crash) still counts it.
                match &m {
                    Move::Runner { unit, .. } => {
                        state.restarts.entry(unit.clone()).or_default().push(now);
                    }
                    Move::Cancel { repo, run, .. } => {
                        state.cancels.insert(format!("{repo}#{run}"), now);
                    }
                }
                save(dir, "state.json", &state)?;
                let done = match &m {
                    Move::Runner { unit, verb, .. } => world.restart(unit, verb),
                    Move::Cancel { repo, run, .. } => world.cancel(repo, *run),
                };
                done.map_or_else(|e| e, |()| "ok".to_owned())
            }
        };
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

/// Whether the move is still called for, looked at again just before it's
/// made: what the pass saw may have moved on while it read the rest. A
/// runner may have taken a job, or a switch begun; a run may have started,
/// or been run again. What can't be read again isn't moved on.
fn still(world: &dyn World, m: &Move) -> Result<(), String> {
    match m {
        Move::Runner { unit, state, .. } => {
            let now = world.unit(unit);
            if world.switching() {
                Err("a switch is in flight".into())
            } else if now.busy {
                Err("it's at a job".into())
            } else if now.state != *state {
                Err(format!("it's {} now", now.state))
            } else {
                Ok(())
            }
        }
        Move::Cancel { repo, run, .. } => {
            let r = world.github(&format!("repos/{repo}/actions/runs/{run}"))?;
            let status = r.get("status").and_then(Value::as_str).unwrap_or("");
            if status != "queued" {
                Err(format!("it's {status} now"))
            } else if r.get("run_attempt").and_then(Value::as_u64) != Some(1) {
                Err("someone ran it again".into())
            } else {
                Ok(())
            }
        }
    }
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
    for (repo, why) in &snap.unknown {
        let _ = writeln!(out, "{repo}: not known to be live: {why}");
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
    use crate::snapshot::tests::{Fake, config, job, page, push, runs};
    use crate::state::State;
    use crate::world::Unit;

    const QUEUED: &str = "repos/sam/r/actions/runs?status=queued";

    fn idle(since: f64) -> Unit {
        Unit {
            state: "active".into(),
            since: Some(since),
            busy: false,
        }
    }

    // A pass end to end: the restart it calls for is kept, then made, and
    // logged; its waits are written for the watchdog, which is kicked only
    // when they change, and its next look is booked.
    #[test]
    fn pass() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = config(dir.path());
        cfg.deploy = None;
        cfg.runners.retain(|u, _| u == "ci");
        let mut w = repo();
        w.github.insert(page(QUEUED, 1), runs(&[1]));
        w.github.insert(
            page("repos/sam/r/actions/runs/1/jobs?filter=latest", 1),
            Ok(json!({"jobs": [job("gate", "queued", "2026-10-29T17:00:00Z", &["self-hosted", "sam"])]})),
        );
        w.units.insert("ci".into(), idle(0.0));
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

        // The same wait: no kick. Its time come: kept, then restarted, and
        // looked at again.
        w.did.borrow_mut().clear();
        w.peek = Some(dir.path().join("state.json"));
        run(&cfg, &w, t + 300.0).unwrap();
        let did = w.did.borrow().clone();
        assert_eq!(did[0], "restart ci");
        let kept: State = serde_json::from_str(&did[1]).unwrap();
        assert_eq!(kept.restarts["ci"], [t + 300.0]);
        assert_eq!(did.len(), 2);
        let st: State = load(dir.path(), "state.json");
        assert_eq!(st.restarts["ci"], [t + 300.0]);
        assert_eq!(st.log.len(), 1);
        assert_eq!(
            (st.log[0].what.as_str(), st.log[0].result.as_str()),
            ("restart ci", "ok")
        );
        let wake: serde_json::Value = load(dir.path(), "wake.json");
        assert_eq!(wake, json!({"": t + 600.0}));
        w.peek = None;

        // One systemd refuses is logged with why, and counted all the same.
        w.fails = Some("Access denied".into());
        w.units.insert("ci".into(), idle(t + 300.0));
        run(&cfg, &w, t + 600.0).unwrap();
        let st: State = load(dir.path(), "state.json");
        assert_eq!(st.restarts["ci"], [t + 300.0, t + 600.0]);
        assert_eq!(st.log[1].result, "Access denied");

        // Taken: nothing waits, and the watchdog hears.
        w.github.insert(page(QUEUED, 1), runs(&[]));
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

    // A runner that has taken a job, or moved on, since the pass looked, or
    // a switch begun, isn't restarted, and the try isn't counted.
    #[test]
    fn moved_on() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = config(dir.path());
        cfg.deploy = None;
        cfg.runners.retain(|u, _| u == "ci");
        let t = 1_793_293_200.0;
        let busy = Unit {
            busy: true,
            ..idle(0.0)
        };
        let stopping = Unit {
            state: "deactivating".into(),
            ..idle(0.0)
        };
        for (then, switching, why) in [
            (busy, false, "it's at a job"),
            (stopping, false, "it's deactivating now"),
            (idle(0.0), true, "a switch is in flight"),
        ] {
            let mut w = repo();
            w.github.insert(page(QUEUED, 1), runs(&[1]));
            w.github.insert(
                page("repos/sam/r/actions/runs/1/jobs?filter=latest", 1),
                Ok(json!({"jobs": [job("gate", "queued", "2026-10-29T16:50:00Z", &["sam"])]})),
            );
            w.units.insert("ci".into(), idle(0.0));
            w.then.insert("ci".into(), then);
            std::fs::write(
                dir.path().join("state.json"),
                json!({"idle": {"ci": t - 600.0}}).to_string(),
            )
            .unwrap();
            // The pass's own look saw no switch; the second one does.
            let w = Switching { w, from: switching };
            run(&cfg, &w, t).unwrap();
            assert!(
                w.w.did.borrow().iter().all(|d| !d.starts_with("restart")),
                "{why}"
            );
            let st: State = load(dir.path(), "state.json");
            assert!(st.restarts.is_empty(), "{why}");
            assert_eq!(st.log[0].result, format!("not made: {why}"));
        }
    }

    /// A machine whose switch begins, when `from`, after its first look.
    struct Switching {
        w: Fake,
        from: bool,
    }

    impl crate::world::World for Switching {
        fn github(&self, path: &str) -> Result<serde_json::Value, String> {
            self.w.github(path)
        }
        fn unit(&self, unit: &str) -> Unit {
            self.w.unit(unit)
        }
        fn running(&self) -> Result<String, String> {
            self.w.running()
        }
        fn switching(&self) -> bool {
            // Its first look is before any unit's second.
            self.from && self.w.looks.borrow().values().any(|n| *n > 1)
        }
        fn restart(&self, unit: &str, verb: &str) -> Result<(), String> {
            self.w.restart(unit, verb)
        }
        fn cancel(&self, repo: &str, run: u64) -> Result<(), String> {
            self.w.cancel(repo, run)
        }
        fn kick(&self, path: &str) {
            self.w.kick(path)
        }
    }

    /// sam/r, on main, with nothing live.
    fn repo() -> Fake {
        let mut w = Fake::default();
        w.github
            .insert("repos/sam/r".into(), Ok(json!({"default_branch": "main"})));
        for status in ["queued", "in_progress", "pending"] {
            w.github.insert(
                page(&format!("repos/sam/r/actions/runs?status={status}"), 1),
                runs(&[]),
            );
        }
        w
    }

    // A moot run, still queued and on its first attempt when looked at
    // again, is cancelled and logged, and tried once, even when GitHub
    // refuses. One that has started, or been run again, since the pass's
    // look, or that can't be looked at again, isn't.
    #[test]
    fn cancel() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = config(dir.path());
        cfg.deploy = None;
        cfg.runners.retain(|u, _| u == "ci");
        let mut w = repo();
        w.github.insert(
            page(QUEUED, 1),
            Ok(json!({"workflow_runs": [push(1, 7, "main", "a1", "queued", "2026-10-29T16:00:00Z")]})),
        );
        w.github.insert(
            page("repos/sam/r/actions/runs?status=pending", 1),
            Ok(json!({"workflow_runs": [push(2, 7, "main", "b2", "pending", "2026-10-29T16:10:00Z")]})),
        );
        w.github.insert(
            page("repos/sam/r/actions/runs/1/jobs?filter=latest", 1),
            Ok(json!({"jobs": []})),
        );
        let t = 1_793_293_200.0;
        let one = "repos/sam/r/actions/runs/1";

        for (now, result) in [
            (
                json!({"status": "in_progress", "run_attempt": 1}),
                "not made: it's in_progress now",
            ),
            (
                json!({"status": "queued", "run_attempt": 2}),
                "not made: someone ran it again",
            ),
        ] {
            w.github.insert(one.into(), Ok(now));
            run(&cfg, &w, t).unwrap();
            assert!(w.did.borrow().is_empty(), "{result}");
            let st: State = load(dir.path(), "state.json");
            assert!(st.cancels.is_empty(), "{result}");
            assert_eq!(st.log.last().unwrap().result, result);
        }
        w.github.remove(one);
        run(&cfg, &w, t).unwrap();
        assert!(w.did.borrow().is_empty());

        w.github.insert(
            one.into(),
            Ok(json!({"status": "queued", "run_attempt": 1})),
        );
        w.fails = Some("HTTP 403: Resource not accessible by integration".into());
        run(&cfg, &w, t).unwrap();
        assert_eq!(*w.did.borrow(), ["cancel sam/r#1"]);
        let st: State = load(dir.path(), "state.json");
        assert_eq!(st.cancels["sam/r#1"], t);
        assert_eq!(
            (
                st.log.last().unwrap().what.as_str(),
                st.log.last().unwrap().result.as_str()
            ),
            (
                "cancel https://github.com/sam/r/actions/runs/1",
                "HTTP 403: Resource not accessible by integration"
            )
        );

        run(&cfg, &w, t + 60.0).unwrap();
        assert_eq!(*w.did.borrow(), ["cancel sam/r#1"]);
    }
}
