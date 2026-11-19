//! One handler's line: the passes due, and when.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Duration;

use serde_json::{Map, Value};

/// A repo's first event waits this long for the ones that follow it.
pub const DEBOUNCE: f64 = 5.0;
/// One repo's passes start at least this far apart, unless the handler
/// sets its own `gap`.
pub const GAP: f64 = 20.0;
/// A pass that failed runs again this long after, doubling each time it
/// fails again, up to RETRY_MAX.
pub const RETRY: f64 = 60.0;
pub const RETRY_MAX: f64 = 3600.0;

/// A pass: the handler's full one, or its pass over one repo. GitHub's
/// names are case-blind, so a repo is kept in lower case: two spellings are
/// one repo's pass.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Pass {
    Full,
    Repo(String),
}

impl Pass {
    pub fn repo(name: &str) -> Pass {
        Pass::Repo(name.to_lowercase())
    }

    /// A key of a wake file: "" is the full pass.
    pub fn key(key: &str) -> Pass {
        if key.is_empty() {
            Pass::Full
        } else {
            Pass::repo(key)
        }
    }
}

impl std::fmt::Display for Pass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Pass::Full => f.write_str("full pass"),
            Pass::Repo(repo) => f.write_str(repo),
        }
    }
}

/// What a pass looks at: everything it covers, or only the issues its events
/// were about (a handler told them, `issueDir`, whose pass had nothing but
/// issue events booked).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    All,
    Issues(BTreeSet<u64>),
}

#[derive(Default)]
struct State {
    /// When each pass is due to look at everything.
    due: HashMap<Pass, f64>,
    /// When each pass is due to look at only some issues, and which: kept
    /// apart from `due`, so a look at everything booked for later (a
    /// wake-up) doesn't turn an issue's event into one now.
    issues: HashMap<Pass, (f64, BTreeSet<u64>)>,
    /// When each pass last started.
    last: HashMap<Pass, f64>,
    /// Passes in a row that failed, by pass.
    fails: HashMap<Pass, u32>,
    /// Something came in since the last take: the wait after it returns at
    /// once instead of sleeping through what it missed.
    news: bool,
    /// Passes taken and not yet done.
    running: usize,
}

pub struct Line {
    /// How far apart one pass's starts are, at least.
    gap: f64,
    state: Mutex<State>,
    cond: Condvar,
}

impl Default for Line {
    fn default() -> Line {
        Line::new(GAP)
    }
}

impl Line {
    pub fn new(gap: f64) -> Line {
        Line {
            gap,
            state: Mutex::default(),
            cond: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // A thread that panicked holding it left the maps whole: each change
        // is one insert or remove.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn book(s: &mut State, pass: Pass, at: f64) {
        let due = s.due.entry(pass).or_insert(at);
        *due = due.min(at);
    }

    /// An event for this pass came in: it looks at everything.
    pub fn event(&self, pass: Pass, now: f64) {
        let mut s = self.lock();
        Self::book(&mut s, pass, now + DEBOUNCE);
        s.news = true;
        self.cond.notify_all();
    }

    /// An event about issue `n` came in for this pass: unless a look at
    /// everything is due first, the pass looks at the burst's issues alone.
    pub fn issue(&self, pass: Pass, n: u64, now: f64) {
        let mut s = self.lock();
        let at = now + DEBOUNCE;
        let (due, ns) = s.issues.entry(pass).or_insert((at, BTreeSet::new()));
        *due = due.min(at);
        ns.insert(n);
        s.news = true;
        self.cond.notify_all();
    }

    /// The wake file: only a wake-up booked after the pass's last start is
    /// new (that pass rewrote its own).
    pub fn wakes(&self, booked: &Map<String, Value>) {
        let mut s = self.lock();
        for (key, at) in booked {
            if !(key.is_empty() || burrow_github::repo_name(key)) {
                continue;
            }
            let pass = Pass::key(key);
            if let Some(at) = at.as_f64()
                && at > s.last.get(&pass).copied().unwrap_or(0.0)
            {
                Self::book(&mut s, pass, at);
            }
        }
        s.news = true;
        self.cond.notify_all();
    }

    /// The passes that start now, each with what it looks at, and how long
    /// until the next one is due (None: nothing is). A pass due to look at
    /// everything sees to its issues' events as well. Each repo's pass comes
    /// before the full one: a full pass may stop short of a repo (the
    /// steward's yields after a slice and resumes), so it never stands in
    /// for one, and however often it's due, a repo's pass waits for no more
    /// than the passes this take starts.
    pub fn take(&self, now: f64) -> (Vec<(Pass, Scope)>, Option<f64>) {
        let mut s = self.lock();
        s.news = false;
        let gap = self.gap;
        let ready_at = |s: &State, pass: &Pass, at: f64| {
            at.max(s.last.get(pass).copied().unwrap_or(0.0) + gap)
        };
        let passes: BTreeSet<Pass> = s.due.keys().chain(s.issues.keys()).cloned().collect();
        let mut ready: Vec<(Pass, Scope)> = passes
            .into_iter()
            .filter_map(|p| {
                if s.due.get(&p).is_some_and(|&at| ready_at(&s, &p, at) <= now) {
                    Some((p, Scope::All))
                } else {
                    let (at, ns) = s.issues.get(&p)?;
                    (ready_at(&s, &p, *at) <= now).then(|| (p, Scope::Issues(ns.clone())))
                }
            })
            .collect();
        ready.sort_by_key(|(p, _)| *p == Pass::Full);
        for (pass, scope) in &ready {
            if *scope == Scope::All {
                s.due.remove(pass);
            }
            s.issues.remove(pass);
            s.last.insert(pass.clone(), now);
        }
        s.running += ready.len();
        let next = s
            .due
            .iter()
            .chain(s.issues.iter().map(|(p, (at, _))| (p, at)))
            .map(|(p, &at)| ready_at(&s, p, at))
            .fold(None, |m: Option<f64>, at| Some(m.map_or(at, |m| m.min(at))));
        (ready, next.map(|at| at - now))
    }

    /// A pass taken that didn't start (its handler yielded first) goes
    /// back: due now, with what it was to look at, and ready as soon as
    /// the line is free again, its GAP already served.
    pub fn untake(&self, pass: &Pass, scope: &Scope, now: f64) {
        let mut s = self.lock();
        s.running = s.running.saturating_sub(1);
        match scope {
            Scope::All => Self::book(&mut s, pass.clone(), now),
            Scope::Issues(ns) => {
                let (due, all) = s
                    .issues
                    .entry(pass.clone())
                    .or_insert((now, BTreeSet::new()));
                *due = due.min(now);
                all.extend(ns);
            }
        }
        s.last.insert(pass.clone(), now - self.gap);
    }

    /// Whether a pass is running, or due by now: a handler that yields to
    /// this one (`yieldTo`) starts none of its own meanwhile.
    pub fn busy(&self, now: f64) -> bool {
        let s = self.lock();
        s.running > 0
            || s.due.values().any(|&at| at <= now)
            || s.issues.values().any(|&(at, _)| at <= now)
    }

    /// A pass ended. One that failed (or never started) runs again a minute
    /// later, then two, up to an hour, looking at everything: no other event
    /// may come for it, and once one goes through its unit isn't failed any
    /// more. Returns when it runs again.
    pub fn done(&self, pass: &Pass, ok: bool, now: f64) -> Option<f64> {
        let mut s = self.lock();
        s.running = s.running.saturating_sub(1);
        if ok {
            s.fails.remove(pass);
            return None;
        }
        let n = s.fails.entry(pass.clone()).or_insert(0);
        *n += 1;
        let after = (RETRY * 2f64.powi((*n - 1).min(31) as i32)).min(RETRY_MAX);
        let at = now + after;
        Self::book(&mut s, pass.clone(), at);
        Some(at)
    }

    /// Whether a pass is running: the handler a yielding one yields to
    /// (`yieldTo`) waits it out, since the two share a lock, and one started
    /// beside it would only find the lock held and come back every GAP.
    pub fn running(&self) -> bool {
        self.lock().running > 0
    }

    /// Hear news: two lines where one yields to the other hear each of the
    /// other's passes end.
    pub fn nudge(&self) {
        self.lock().news = true;
        self.cond.notify_all();
    }

    /// Whether `held` says another line holds this one off now (the one it
    /// yields to, busy, or one yielding to it, running); if so, wait for
    /// news (one of that line's passes ending, say). The news is cleared
    /// before `held` is asked, so a pass ending after that is never slept
    /// through.
    pub fn hold(&self, held: impl FnOnce() -> bool) -> bool {
        self.lock().news = false;
        let held = held();
        if held {
            self.wait(None);
        }
        held
    }

    /// Wait for news, or this long (None: until news comes).
    pub fn wait(&self, timeout: Option<f64>) {
        let s = self.lock();
        if s.news {
            return;
        }
        // Poisoned only if a thread panicked holding the lock: nothing to
        // wait for then.
        match timeout {
            None => drop(self.cond.wait_while(s, |s| !s.news)),
            Some(t) => drop(self.cond.wait_timeout_while(
                s,
                Duration::from_secs_f64(t.max(0.0)),
                |s| !s.news,
            )),
        }
    }

    #[cfg(test)]
    pub fn due(&self) -> HashMap<Pass, f64> {
        self.lock().due.clone()
    }

    #[cfg(test)]
    pub fn clear(&self) {
        let mut s = self.lock();
        s.due.clear();
        s.issues.clear();
    }

    #[cfg(test)]
    pub fn issues(&self) -> HashMap<Pass, (f64, BTreeSet<u64>)> {
        self.lock().issues.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    use serde_json::{Map, Value, json};

    use super::{DEBOUNCE, Line, Pass, Scope};

    /// The passes that start now, every one looking at everything.
    fn took(line: &Line, now: f64) -> (Vec<Pass>, Option<f64>) {
        let (ready, next) = line.take(now);
        let passes = ready
            .into_iter()
            .map(|(p, scope)| {
                assert_eq!(scope, Scope::All, "{p}");
                p
            })
            .collect();
        (passes, next)
    }

    fn booked(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    fn due(pairs: &[(Pass, f64)]) -> HashMap<Pass, f64> {
        pairs.iter().cloned().collect()
    }

    fn r(name: &str) -> Pass {
        Pass::repo(name)
    }

    // A burst of events is one pass, a few seconds on; one repo's passes are
    // GAP apart; only a wake-up booked since its last pass started counts.
    #[test]
    fn bursts_gaps_and_wakes() {
        let line = Line::default();
        line.event(r("sam/r"), 100.0);
        line.event(r("sam/r"), 102.0);
        assert_eq!(took(&line, 104.0), (vec![], Some(1.0)));
        assert_eq!(took(&line, 105.0), (vec![r("sam/r")], None));
        assert_eq!(took(&line, 106.0), (vec![], None));
        line.event(r("sam/r"), 110.0);
        assert_eq!(took(&line, 115.0), (vec![], Some(10.0)));
        assert_eq!(took(&line, 125.0).0, vec![r("sam/r")]);
        line.wakes(&booked(
            json!({"sam/r": 120, "sam/other": 200, "--bad": 130, "sam/q": "soon", "sam/t": true}),
        ));
        assert_eq!(line.due(), due(&[(r("sam/other"), 200.0)]));
        line.wakes(&booked(json!({"sam/r": 160})));
        assert_eq!(took(&line, 159.0), (vec![], Some(1.0)));
        assert_eq!(took(&line, 160.0).0, vec![r("sam/r")]);
    }

    // A handler's own gap holds its passes apart, wake-ups and all.
    #[test]
    fn own_gap() {
        let line = Line::new(120.0);
        line.event(Pass::Full, 1000.0);
        assert_eq!(took(&line, 1005.0).0, vec![Pass::Full]);
        line.event(Pass::Full, 1010.0);
        assert_eq!(took(&line, 1025.0), (vec![], Some(100.0)));
        line.wakes(&booked(json!({"": 1050})));
        assert_eq!(took(&line, 1124.0), (vec![], Some(1.0)));
        assert_eq!(took(&line, 1125.0).0, vec![Pass::Full]);
    }

    // A full pass may stop short of a repo (the steward's yields after a
    // slice and resumes), so it never stands in for one: a repo's pass due
    // runs beside it, first, and one held off by its GAP stays booked.
    #[test]
    fn full_pass_stands_in_for_none() {
        let line = Line::default();
        line.event(r("sam/r"), 100.0);
        took(&line, 105.0);
        line.event(r("sam/r"), 106.0);
        line.event(r("sam/q"), 106.0);
        line.event(Pass::Full, 106.0 - DEBOUNCE);
        line.wakes(&booked(json!({"sam/s": 500})));
        assert_eq!(
            took(&line, 111.0),
            (vec![r("sam/q"), Pass::Full], Some(14.0))
        );
        assert_eq!(
            line.due(),
            due(&[(r("sam/r"), 106.0 + DEBOUNCE), (r("sam/s"), 500.0)])
        );
        assert_eq!(took(&line, 125.0), (vec![r("sam/r")], Some(375.0)));
    }

    // The steward's full pass, kicked again during each of its slices (a
    // job's turn, a stall's notice), is due at every take. A repo kicked or
    // woken meanwhile still gets its pass in the next take, not once the
    // traversal is through: no swallowed kick leaves it unvisited for hours.
    #[test]
    fn full_pass_due_again_holds_off_no_repo() {
        let line = Line::default();
        line.event(Pass::Full, 100.0);
        let mut t = 100.0 + DEBOUNCE;
        assert_eq!(took(&line, t).0, vec![Pass::Full]);
        let slice = 480.0;
        for n in 0..6 {
            line.event(Pass::Full, t + 10.0);
            if n % 2 == 0 {
                // A review came back: its repo's kick.
                line.event(r("sam/c"), t + 20.0);
            } else {
                // A wake-up the last pass booked for just after it.
                line.wakes(&booked(json!({"sam/c": t + slice + 1.0})));
            }
            t += slice + 1.0;
            assert_eq!(took(&line, t).0, vec![r("sam/c"), Pass::Full], "slice {n}");
        }
    }

    // A handler that yields to this line holds off while one of its passes
    // runs or is due, and not once it has none; the one it yields to waits
    // out only a pass of the yielder's that runs.
    #[test]
    fn busy_while_due_or_running() {
        let line = Line::default();
        assert!(!line.busy(100.0));
        line.wakes(&booked(json!({"sam/r": 200})));
        assert!(!line.busy(199.0));
        assert!(line.busy(200.0));
        assert!(!line.running(), "due isn't running");
        assert_eq!(took(&line, 200.0).0, vec![r("sam/r")]);
        assert!(line.busy(200.0), "its pass is running");
        assert!(line.running());
        line.done(&r("sam/r"), true, 300.0);
        assert!(!line.busy(300.0));
        assert!(!line.running());
        line.issue(r("sam/r"), 7, 300.0);
        assert!(line.busy(300.0 + DEBOUNCE));
    }

    // A line held off by another sleeps until news comes, and isn't held
    // once that one is idle; news from before the hold isn't news.
    #[test]
    fn held() {
        let first = Arc::new(Line::default());
        let line = Arc::new(Line::default());
        line.event(r("sam/r"), 0.0);
        assert!(!line.hold(|| first.busy(100.0)));
        first.event(r("sam/q"), 0.0);
        line.event(r("sam/r"), 1.0);
        let waiting = {
            let (first, line) = (first.clone(), line.clone());
            thread::spawn(move || line.hold(|| first.busy(100.0)))
        };
        thread::sleep(Duration::from_millis(200));
        assert!(!waiting.is_finished(), "old news ended the hold");
        line.nudge();
        assert!(waiting.join().unwrap());
    }

    // GitHub's names are case-blind: two spellings are one repo's pass.
    #[test]
    fn case_blind() {
        let line = Line::default();
        line.event(r("Sam/R"), 100.0);
        line.event(r("sam/r"), 101.0);
        line.wakes(&booked(json!({"SAM/r": 103})));
        assert_eq!(took(&line, 106.0), (vec![r("sam/r")], None));
    }

    // A pass can book its handler's full pass too ("": the steward's, after
    // a rate limit turned it away).
    #[test]
    fn booked_full_pass() {
        let line = Line::default();
        line.wakes(&booked(json!({"": 300})));
        assert_eq!(line.due(), due(&[(Pass::Full, 300.0)]));
        assert_eq!(took(&line, 300.0).0, vec![Pass::Full]);
    }

    // An event between a take that found nothing and the wait after it isn't
    // slept through.
    #[test]
    fn no_lost_news() {
        let line = Arc::new(Line::default());
        assert_eq!(took(&line, 0.0), (vec![], None));
        line.event(r("sam/r"), 0.0);
        let waiting = {
            let line = line.clone();
            thread::spawn(move || line.wait(None))
        };
        thread::sleep(Duration::from_millis(200));
        assert!(
            waiting.is_finished(),
            "the wait slept through an event that came in before it"
        );
    }

    // A wait wakes for an event that comes in during it.
    #[test]
    fn woken() {
        let line = Arc::new(Line::default());
        took(&line, 0.0);
        let waiting = {
            let line = line.clone();
            thread::spawn(move || line.wait(Some(60.0)))
        };
        thread::sleep(Duration::from_millis(100));
        line.event(r("sam/r"), 0.0);
        thread::sleep(Duration::from_millis(200));
        assert!(waiting.is_finished(), "an event didn't end the wait");
    }

    // A pass that failed runs again a minute later, then two, up to an hour,
    // and from scratch once one goes through.
    #[test]
    fn retries() {
        let line = Line::default();
        let got: Vec<_> = (0..8).map(|_| line.done(&Pass::Full, false, 0.0)).collect();
        assert_eq!(
            got,
            [60.0, 120.0, 240.0, 480.0, 960.0, 1920.0, 3600.0, 3600.0].map(Some)
        );
        assert_eq!(
            line.due(),
            due(&[(Pass::Full, 60.0)]),
            "a retry never puts a due pass off"
        );
        assert_eq!(line.done(&Pass::Full, true, 0.0), None);
        assert_eq!(line.done(&Pass::Full, false, 0.0), Some(60.0));
        assert_eq!(line.done(&r("sam/r"), false, 0.0), Some(60.0));
    }

    // An issue's events are a look at those issues alone, a burst's
    // together; a look at everything booked for later (a wake-up) stays
    // booked, and the repo's own due now takes them in, which a full pass
    // doesn't stand in for. A pass that failed runs again looking at
    // everything.
    #[test]
    fn issues() {
        let line = Line::default();
        line.wakes(&booked(json!({"sam/r": 500})));
        line.issue(r("sam/r"), 7, 100.0);
        line.issue(r("Sam/R"), 9, 102.0);
        line.issue(r("sam/q"), 7, 102.0);
        assert_eq!(line.take(104.0), (vec![], Some(1.0)));
        assert_eq!(
            line.take(105.0),
            (vec![(r("sam/r"), Scope::Issues([7, 9].into()))], Some(2.0))
        );
        assert_eq!(line.due(), due(&[(r("sam/r"), 500.0)]));
        assert_eq!(
            line.take(107.0).0,
            vec![(r("sam/q"), Scope::Issues([7].into()))]
        );
        line.issue(r("sam/r"), 3, 200.0);
        line.event(r("sam/r"), 200.0);
        assert_eq!(line.take(205.0), (vec![(r("sam/r"), Scope::All)], None));
        assert!(line.issues().is_empty());
        line.issue(r("sam/r"), 4, 300.0);
        line.event(Pass::Full, 300.0);
        assert_eq!(
            line.take(305.0),
            (
                vec![
                    (r("sam/r"), Scope::Issues([4].into())),
                    (Pass::Full, Scope::All)
                ],
                None
            )
        );
        assert!(line.issues().is_empty());
        line.issue(r("sam/r"), 5, 400.0);
        assert_eq!(
            line.take(405.0).0,
            vec![(r("sam/r"), Scope::Issues([5].into()))]
        );
        line.done(&r("sam/r"), false, 406.0);
        assert_eq!(line.due(), due(&[(r("sam/r"), 466.0)]));
        assert!(line.issues().is_empty());
    }
}
