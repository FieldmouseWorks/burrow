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
    // Before this pass hands anything over: what no unit has now was left
    // by an earlier start that never ran.
    if let Some(f) = &cfg.fresh {
        for said in world.sweep(f) {
            println!("{said}");
        }
    }
    let snap = snapshot::take(cfg, world);
    for (repo, err) in &snap.unread {
        println!("couldn't read {repo}: {err}");
    }
    if let Some(Err(e)) = &snap.fresh {
        println!("couldn't list the fresh runners, so none is started or stopped: {e}");
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
                    Move::PrivateFresh { .. } | Move::Fresh { .. } | Move::Retire { .. } => {}
                }
                save(dir, "state.json", &state)?;
                let done = match &m {
                    Move::Runner { unit, verb, .. } => world.restart(unit, verb),
                    Move::Cancel { repo, run, .. } => world.cancel(repo, *run),
                    Move::Fresh { instance, .. } => match &cfg.fresh {
                        Some(f) => world.fresh(f, instance),
                        None => Err("the machine has no fresh runners".into()),
                    },
                    Move::PrivateFresh { repo, instance, .. } => {
                        match snap
                            .private
                            .iter()
                            .find(|(f, _)| f.repository.as_ref() == Some(repo))
                        {
                            Some((f, _)) => world.fresh(f, instance),
                            None => Err("private repository was not discovered".into()),
                        }
                    }
                    Move::Retire { unit, .. } => world.restart(unit, "stop"),
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
    // A run's deadline is read from its logs between passes, so the watchdog
    // waits on each pass for these (watchdog's `refresh`).
    let changed = items != was || items.values().any(|i| i.refresh);
    save(
        dir,
        "watch.json",
        &File {
            at: now,
            items: items.clone(),
        },
    )?;
    // What's new may fall due before the watchdog's booked pass, and what
    // has moved on is no stall.
    if let (true, Some(w)) = (changed, &cfg.watchdog) {
        world.kick(w);
    }
    let wake: BTreeMap<&str, f64> = wake.into_iter().map(|t| ("", t)).collect();
    save(dir, "wake.json", &wake)?;
    if cfg.private.is_some() {
        let current = Decision {
            items,
            ..Decision::default()
        };
        save(
            dir,
            "status.json",
            &SavedStatus {
                at: now,
                report: render(cfg, &snap, &state, &current, false),
            },
        )?;
    }
    Ok(())
}

/// Whether the move is still called for, looked at again just before it's
/// made: what the pass saw may have moved on while it read the rest. A
/// runner may have taken a job, or a switch begun; a run may have finished,
/// been run again, or lost the run after it. What can't be read again isn't
/// moved on.
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
        // A start that turns out to be one too many is a runner stopped
        // `quiet` later.
        Move::Fresh { .. } | Move::PrivateFresh { .. } => Ok(()),
        Move::Retire { unit, .. } => {
            let now = world.unit(unit);
            if now.busy {
                Err("it's at a job".into())
            } else if now.state != "active" {
                Err(format!("it's {} now", now.state))
            } else {
                Ok(())
            }
        }
        // Started or not, a run still unfinished on its first attempt is
        // cancelled; one that finished, or was run again, is left. So is one
        // whose later run was cancelled meanwhile: it's the branch's result
        // again. The later run is read first, so the run itself is read
        // last, right before the cancel: a read can wait out GitHub's
        // retries, and the run may finish and be run again meanwhile, when
        // the cancel would take its new attempt.
        Move::Cancel {
            repo, run, later, ..
        } => {
            let l = world.github(&format!("repos/{repo}/actions/runs/{later}"))?;
            let status = l.get("status").and_then(Value::as_str).unwrap_or("");
            let conclusion = l.get("conclusion").and_then(Value::as_str);
            if status.is_empty() {
                return Err(format!("the run after it, {later}, has no status"));
            }
            if status == "completed" && conclusion == Some("cancelled") {
                return Err(format!("the run after it, {later}, was cancelled"));
            }
            let r = world.github(&format!("repos/{repo}/actions/runs/{run}"))?;
            let status = r.get("status").and_then(Value::as_str).unwrap_or("");
            if status.is_empty() || status == "completed" {
                Err(format!("it's {} now", or_unread(status)))
            } else if r.get("run_attempt").and_then(Value::as_u64) != Some(1) {
                Err("someone ran it again".into())
            } else {
                Ok(())
            }
        }
    }
}

fn or_unread(status: &str) -> &str {
    if status.is_empty() {
        "without a status"
    } else {
        status
    }
}

/// What it sees and waits on, with the moves it has made: changing nothing.
/// Private discovery needs the service's credential, so shell status reads
/// its timestamped report instead of trying to mint an installation token.
pub fn status(cfg: &Config, world: &dyn World, now: f64) -> String {
    if cfg.private.is_some() {
        return match std::fs::read(cfg.state_dir.join("status.json"))
            .map_err(|e| e.to_string())
            .and_then(|b| serde_json::from_slice::<SavedStatus>(&b).map_err(|e| e.to_string()))
        {
            Ok(saved) => format!(
                "dispatch snapshot at {} ({} seconds ago):\n{}",
                clock(saved.at),
                (now - saved.at).max(0.0) as u64,
                saved.report
            ),
            Err(e) => format!(
                "no saved dispatch status; the service has not published a readable snapshot: {e}"
            ),
        };
    }
    let dir = &cfg.state_dir;
    let st: State = load(dir, "state.json");
    let was = watch::read(&dir.join("watch.json"))
        .map(|(_, items)| items)
        .unwrap_or_default();
    let snap = snapshot::take(cfg, world);
    let d = decide(cfg, &snap, &st, &was, now);
    render(cfg, &snap, &st, &d, true)
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SavedStatus {
    at: f64,
    report: String,
}

fn render(cfg: &Config, snap: &Snapshot, st: &State, d: &Decision, planned: bool) -> String {
    let mut out = String::new();
    seen(&mut out, cfg, snap);
    for it in d.items.values() {
        let _ = writeln!(
            out,
            "waiting: {}: {}, due {}",
            line(it),
            it.phase,
            clock(it.due)
        );
    }
    for m in d.moves.iter().filter(|_| planned) {
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
    for (f, pool) in &snap.private {
        match pool {
            Ok(p) => {
                let _ = writeln!(
                    out,
                    "private runners ({}): {} up; {} queued",
                    f.owner,
                    p.units.len(),
                    p.queued.len()
                );
            }
            Err(e) => {
                let _ = writeln!(out, "private runners ({}): {e}", f.owner);
            }
        }
    }
    if let (Some(f), Some(Err(e))) = (&cfg.fresh, &snap.fresh) {
        let _ = writeln!(out, "fresh runners ({}): {e}", f.label);
    }
    if let (Some(f), Some(Ok(pool))) = (&cfg.fresh, &snap.fresh) {
        let busy = pool.units.values().filter(|u| u.busy).count();
        let _ = writeln!(
            out,
            "fresh runners ({}): {} up, {busy} at a job, of {} at most; {} queued for them",
            f.label,
            pool.units.len(),
            f.most,
            pool.queued.len()
        );
        for (i, u) in &pool.units {
            let _ = writeln!(
                out,
                "  {}{i}: {}{}{}",
                f.prefix,
                u.state,
                if u.pending { ", a job queued" } else { "" },
                if u.busy { ", at a job" } else { "" }
            );
        }
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
    for (repo, commit) in cfg.deploy.iter().flat_map(|d| &d.held) {
        let _ = writeln!(
            out,
            "{repo}: held at {} by the flake's own rev, so its default branch isn't going live",
            commit.get(..7).unwrap_or(commit)
        );
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
            ..Unit::default()
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

    // A job queued for the fresh runners gets one registered and started,
    // logged; one left idle is stopped, unless it took a job meanwhile.
    #[test]
    fn fresh_runners() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = config(dir.path());
        cfg.deploy = None;
        cfg.runners.retain(|u, _| u == "ci");
        cfg.fresh = Some(crate::snapshot::tests::fresh(dir.path()));
        let mut w = repo();
        w.github
            .insert("repos/sam/f".into(), Ok(json!({"default_branch": "main"})));
        for status in ["queued", "in_progress", "pending"] {
            w.github.insert(
                page(&format!("repos/sam/f/actions/runs?status={status}"), 1),
                runs(&[]),
            );
        }
        w.github.insert(page(QUEUED, 1), runs(&[1]));
        w.github.insert(
            page("repos/sam/r/actions/runs/1/jobs?filter=latest", 1),
            Ok(json!({"jobs": [job("train", "queued", "2026-10-29T17:00:00Z", &["fresh-box"])]})),
        );
        let t = 1_793_293_200.0;
        run(&cfg, &w, t).unwrap();
        let sweep = format!("sweep {}", dir.path().join("configs").display());
        assert_eq!(
            *w.did.borrow(),
            [
                sweep.as_str(),
                "fresh fresh-box-1793293200-0 for sam",
                "kick watchdog"
            ]
        );
        let st: State = load(dir.path(), "state.json");
        assert_eq!(
            (st.log[0].what.as_str(), st.log[0].result.as_str()),
            ("start fresh runner 1793293200-0", "ok")
        );

        // Up since, and idle: stopped once it has had its `quiet`, unless
        // it's at a job by the second look.
        w.instances = vec!["1793293200-0".into()];
        let unit = "fresh-runner@1793293200-0.service";
        w.units.insert(unit.into(), idle(t));
        w.github.insert(page(QUEUED, 1), runs(&[]));
        w.did.borrow_mut().clear();
        run(&cfg, &w, t + 300.0).unwrap();
        assert_eq!(w.did.borrow()[1], format!("stop {unit}"));
        w.then.insert(
            unit.into(),
            Unit {
                busy: true,
                ..idle(t)
            },
        );
        w.looks.borrow_mut().clear();
        w.did.borrow_mut().clear();
        run(&cfg, &w, t + 600.0).unwrap();
        assert!(w.did.borrow().iter().all(|d| !d.starts_with("stop")));
        let st: State = load(dir.path(), "state.json");
        assert_eq!(st.log.last().unwrap().result, "not made: it's at a job");
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
        fn instances(&self, template: &str) -> Result<Vec<String>, String> {
            self.w.instances(template)
        }
        fn fresh(&self, f: &crate::config::Fresh, instance: &str) -> Result<(), String> {
            self.w.fresh(f, instance)
        }
        fn sweep(&self, f: &crate::config::Fresh) -> Vec<String> {
            self.w.sweep(f)
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

    // A moot run, still unfinished and on its first attempt when looked at
    // again, with the run after it not cancelled meanwhile, is cancelled and
    // logged, and tried once, even when GitHub refuses: whether it's still
    // queued, has started, or is next in line. One that has finished, or
    // been run again, since the pass's look, whose later run was cancelled,
    // or either of which can't be looked at again, isn't.
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
        let two = "repos/sam/r/actions/runs/2";
        let first = |status: &str, attempt: u64| json!({"status": status, "run_attempt": attempt});
        let pending = json!({"status": "pending", "conclusion": null});

        for (now, later, result) in [
            (
                Some(json!({"status": "completed", "conclusion": "success", "run_attempt": 1})),
                Some(pending.clone()),
                "not made: it's completed now",
            ),
            (
                Some(first("queued", 2)),
                Some(pending.clone()),
                "not made: someone ran it again",
            ),
            (
                Some(json!({"run_attempt": 1})),
                Some(pending.clone()),
                "not made: it's without a status now",
            ),
            (
                None,
                Some(pending.clone()),
                "not made: GET repos/sam/r/actions/runs/1: HTTP 404",
            ),
            (
                Some(first("in_progress", 1)),
                Some(json!({"status": "completed", "conclusion": "cancelled"})),
                "not made: the run after it, 2, was cancelled",
            ),
            (
                Some(first("in_progress", 1)),
                None,
                "not made: GET repos/sam/r/actions/runs/2: HTTP 404",
            ),
        ] {
            for (path, v) in [(one, now), (two, later)] {
                match v {
                    Some(v) => w.github.insert(path.into(), Ok(v)),
                    None => w.github.remove(path),
                };
            }
            run(&cfg, &w, t).unwrap();
            assert!(w.did.borrow().is_empty(), "{result}");
            let st: State = load(dir.path(), "state.json");
            assert!(st.cancels.is_empty(), "{result}");
            assert_eq!(st.log.last().unwrap().result, result);
        }

        // Moved on since the look, from queued to started or next in line,
        // and the run after it under way or done: still moot.
        for (i, (now, later)) in [
            (first("in_progress", 1), json!({"status": "in_progress"})),
            (
                first("pending", 1),
                json!({"status": "completed", "conclusion": "failure"}),
            ),
            (first("queued", 1), pending.clone()),
        ]
        .into_iter()
        .enumerate()
        {
            let dir = tempfile::tempdir().unwrap();
            let mut cfg = cfg.clone();
            cfg.state_dir = dir.path().to_owned();
            w.github.insert(one.into(), Ok(now));
            w.github.insert(two.into(), Ok(later));
            w.did.borrow_mut().clear();
            run(&cfg, &w, t).unwrap();
            assert_eq!(*w.did.borrow(), ["cancel sam/r#1"], "{i}");
            let st: State = load(dir.path(), "state.json");
            assert_eq!(st.log.last().unwrap().result, "ok", "{i}");
        }

        w.did.borrow_mut().clear();
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

    // A run that finishes, or finishes and is run again, while the run after
    // it is read (a read that may wait out GitHub's retries) isn't cancelled,
    // whether it was queued, next in line or under way when the pass looked:
    // the cancel would take its new attempt.
    #[test]
    fn cancel_moved_during_later_read() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = config(dir.path());
        cfg.deploy = None;
        cfg.runners.retain(|u, _| u == "ci");
        let one = "repos/sam/r/actions/runs/1";
        let two = "repos/sam/r/actions/runs/2";
        let t = 1_793_293_200.0;
        for was in ["queued", "pending", "in_progress"] {
            for (became, result) in [
                (
                    json!({"status": "completed", "conclusion": "success", "run_attempt": 1}),
                    "not made: it's completed now",
                ),
                (
                    json!({"status": "queued", "run_attempt": 2}),
                    "not made: someone ran it again",
                ),
            ] {
                let mut w = repo();
                w.github.insert(
                    page(&format!("repos/sam/r/actions/runs?status={was}"), 1),
                    Ok(json!({"workflow_runs": [push(1, 7, "main", "a1", was, "2026-10-29T16:00:00Z")]})),
                );
                let pending = page("repos/sam/r/actions/runs?status=pending", 1);
                let mut listed = w.github[&pending].clone().unwrap();
                listed["workflow_runs"].as_array_mut().unwrap().push(push(
                    2,
                    7,
                    "main",
                    "b2",
                    "pending",
                    "2026-10-29T16:10:00Z",
                ));
                w.github.insert(pending, Ok(listed));
                w.github.insert(
                    page("repos/sam/r/actions/runs/1/jobs?filter=latest", 1),
                    Ok(json!({"jobs": []})),
                );
                w.github
                    .insert(one.into(), Ok(json!({"status": was, "run_attempt": 1})));
                w.github.insert(
                    two.into(),
                    Ok(json!({"status": "pending", "conclusion": null})),
                );
                w.moves.insert(two.into(), (one.into(), became));
                run(&cfg, &w, t).unwrap();
                assert!(w.did.borrow().is_empty(), "{was}: {result}");
                let st: State = load(dir.path(), "state.json");
                assert!(st.cancels.is_empty(), "{was}: {result}");
                assert_eq!(st.log.last().unwrap().result, result, "{was}");
            }
        }
    }

    // An input the flake holds at a commit: status says so, where a watched
    // one says how far behind live it is, and no wait is kept for it.
    #[test]
    fn held_input() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(dir.path());
        let w = Fake::default();
        let said = super::status(&cfg, &w, 1_794_656_000.0);
        assert!(
            said.contains(
                "sam/pin: held at ddddddd by the flake's own rev, so its default branch isn't going live\n"
            ),
            "{said}"
        );
        run(&cfg, &w, 1_794_656_000.0).unwrap();
        let (_, items) = watch::read(&dir.path().join("watch.json")).unwrap();
        assert!(items.keys().all(|k| !k.contains("sam/pin")), "{items:?}");
    }

    // 9c6ad02 and fceda93 both under way, the first stuck in a gate leg for
    // hours. The pass cancels the first for the second, and nothing else:
    // not the second, not code-scanning's runs (another workflow, the
    // earlier one finished), not a push to another branch, and not a PR's
    // run.
    #[test]
    fn cancel_running_push() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = config(dir.path());
        cfg.deploy = None;
        cfg.runners.retain(|u, _| u == "ci");
        let mut w = repo();
        let flake = 394_634_593;
        let scan = 417_777_930;
        let (stuck, next) = (43_577_282_863_u64, 43_584_281_458_u64);
        let pr = json!({"id": 43_584_000_001_u64, "event": "pull_request", "workflow_id": flake,
                        "head_branch": "main", "head_sha": "e5e5e5e", "status": "in_progress",
                        "created_at": "2026-11-14T08:00:00Z", "run_attempt": 1});
        w.github.insert(
            page("repos/sam/r/actions/runs?status=in_progress", 1),
            Ok(json!({"workflow_runs": [
                pr,
                push(43_576_000_002, flake, "fix", "f6f6f6f", "in_progress", "2026-11-14T08:30:00Z"),
                push(stuck, flake, "main", "9c6ad0248c58f0fe1d5fcc6bc6d97f72361502b9", "in_progress", "2026-11-14T08:45:03Z"),
                push(43_584_281_400, scan, "main", "fceda93f1e45e7377abf370b750178ac78eef56f", "in_progress", "2026-11-14T10:13:51Z"),
                push(next, flake, "main", "fceda93f1e45e7377abf370b750178ac78eef56f", "in_progress", "2026-11-14T10:13:51Z"),
                push(43_590_000_003, flake, "fix", "a7a7a7a", "in_progress", "2026-11-14T10:20:00Z"),
            ]})),
        );
        for id in [
            43_584_000_001_u64,
            43_576_000_002,
            stuck,
            43_584_281_400,
            next,
            43_590_000_003,
        ] {
            w.github.insert(
                page(
                    &format!("repos/sam/r/actions/runs/{id}/jobs?filter=latest"),
                    1,
                ),
                Ok(json!({"jobs": []})),
            );
            w.github.insert(
                format!("repos/sam/r/actions/runs/{id}"),
                Ok(json!({"status": "in_progress", "run_attempt": 1})),
            );
        }
        let t = 1_794_656_000.0;
        run(&cfg, &w, t).unwrap();
        assert_eq!(*w.did.borrow(), [format!("cancel sam/r#{stuck}")]);
        let st: State = load(dir.path(), "state.json");
        assert_eq!(
            st.cancels.keys().collect::<Vec<_>>(),
            [&format!("sam/r#{stuck}")]
        );
        assert_eq!(
            st.log.last().unwrap().why,
            format!(
                "it checks 9c6ad02 on main and is under way, and \
                 https://github.com/sam/r/actions/runs/{next} checks fceda93, which came after it"
            )
        );
    }
}
