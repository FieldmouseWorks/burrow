//! One handler's line: the passes due, and when.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Duration;

use serde_json::{Map, Value};

use crate::told::{First, Told};

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
/// one repo's pass. The full pass sorts first.
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

/// What a pass looks at: everything it covers, or what its events were
/// about (a handler told them, `issueDir` or `prDir`, whose pass had
/// nothing but such events booked): told.rs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    All,
    Told(BTreeSet<Told>),
}

#[derive(Default)]
struct State {
    /// When each pass is due to look at everything.
    due: HashMap<Pass, f64>,
    /// When each pass is due to look at only what it's told, and what:
    /// kept apart from `due`, so a look at everything booked for later (a
    /// wake-up) doesn't turn a told event into one now.
    told: HashMap<Pass, (f64, BTreeSet<Told>)>,
    /// The passes booked for an event about a PR that goes first (`firsts`):
    /// they start ahead of every other, the full pass's too.
    first: HashSet<Pass>,
    /// The PRs that go first, by repo, as the handler's passes last listed
    /// them.
    firsts: BTreeMap<String, Vec<First>>,
    /// When each pass last started.
    last: HashMap<Pass, f64>,
    /// Passes in a row that failed, by pass.
    fails: HashMap<Pass, u32>,
    /// Something came in since the last take: the wait after it returns at
    /// once instead of sleeping through what it missed.
    news: bool,
}

pub struct Line {
    /// How far apart one pass's starts are, at least.
    gap: f64,
    /// A file that's there while a pass for a first PR waits to start, for
    /// the handler's passes to see (`firstMarker`): one running yields to it.
    marker: Option<PathBuf>,
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
            marker: None,
            state: Mutex::default(),
            cond: Condvar::new(),
        }
    }

    /// With the file that says a first PR's pass waits (`marker`).
    pub fn with_marker(mut self, marker: Option<PathBuf>) -> Line {
        self.marker = marker;
        // Whatever an earlier run left says nothing of this one's line.
        self.mark(false);
        self
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // A thread that panicked holding it left the maps whole: each change
        // is one insert or remove.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The marker there or not, as `first` is: called holding the lock, so
    /// what it says and the line agree.
    fn mark(&self, waiting: bool) {
        let Some(m) = &self.marker else { return };
        let done = if waiting {
            m.parent()
                .map_or(Ok(()), fs::create_dir_all)
                .and_then(|()| fs::write(m, ""))
        } else {
            match fs::remove_file(m) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            }
        };
        if let Err(e) = done {
            eprintln!("{}: {e}", m.display());
        }
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

    /// An event about these (issues, PRs, branches, commits) came in for
    /// this pass: unless a look at everything is due first, the pass looks
    /// at the burst's alone. One about a PR that goes first puts the pass
    /// ahead of the rest. `only_first`: an event that counts only for one
    /// (`firstOnly`), booking nothing otherwise. Whether it was for one.
    pub fn told(&self, pass: Pass, items: Vec<Told>, now: f64, only_first: bool) -> bool {
        let mut s = self.lock();
        let first = match &pass {
            Pass::Repo(repo) => s
                .firsts
                .get(repo)
                .is_some_and(|fs| items.iter().any(|t| fs.iter().any(|f| f.matches(t)))),
            Pass::Full => false,
        };
        if only_first && !first {
            return false;
        }
        let at = now + DEBOUNCE;
        let (due, ns) = s.told.entry(pass.clone()).or_insert((at, BTreeSet::new()));
        *due = due.min(at);
        ns.extend(items);
        if first {
            s.first.insert(pass);
            self.mark(true);
        }
        s.news = true;
        self.cond.notify_all();
        first
    }

    /// The PRs that go first, as the handler's passes list them
    /// (told::firsts), replacing the last list.
    pub fn firsts(&self, firsts: BTreeMap<String, Vec<First>>) {
        self.lock().firsts = firsts;
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
    /// until the next one is due (None: nothing is). Those for a first PR
    /// come before anything else, alone; then the full pass, which sees to
    /// every repo whose event has come in as well; then the rest. A pass
    /// due to look at everything sees to what it's told as well.
    pub fn take(&self, now: f64) -> (Vec<(Pass, Scope)>, Option<f64>) {
        let mut s = self.lock();
        s.news = false;
        let gap = self.gap;
        let ready_at = |s: &State, pass: &Pass, at: f64| {
            at.max(s.last.get(pass).copied().unwrap_or(0.0) + gap)
        };
        let passes: BTreeSet<Pass> = s.due.keys().chain(s.told.keys()).cloned().collect();
        let mut ready: Vec<(Pass, Scope)> = passes
            .into_iter()
            .filter_map(|p| {
                if s.due.get(&p).is_some_and(|&at| ready_at(&s, &p, at) <= now) {
                    Some((p, Scope::All))
                } else {
                    let (at, ns) = s.told.get(&p)?;
                    (ready_at(&s, &p, *at) <= now).then(|| (p, Scope::Told(ns.clone())))
                }
            })
            .collect();
        if ready.iter().any(|(p, _)| s.first.contains(p)) {
            // A first PR's waits for nothing else: what else is due, the
            // full pass too, stays due for the next take.
            ready.retain(|(p, _)| s.first.contains(p));
        } else if ready.first().is_some_and(|(p, _)| *p == Pass::Full) {
            // It sees to every repo whose event has come in as well, GAP or
            // not.
            let seen: Vec<Pass> = s
                .due
                .iter()
                .chain(s.told.iter().map(|(p, (at, _))| (p, at)))
                .filter(|&(p, &at)| *p != Pass::Full && at <= now && !s.first.contains(p))
                .map(|(p, _)| p.clone())
                .collect();
            for pass in seen {
                s.due.remove(&pass);
                s.told.remove(&pass);
                s.last.insert(pass, now);
            }
            ready = vec![(Pass::Full, Scope::All)];
        }
        for (pass, scope) in &ready {
            if *scope == Scope::All {
                s.due.remove(pass);
            }
            s.told.remove(pass);
            s.first.remove(pass);
            s.last.insert(pass.clone(), now);
        }
        if !ready.is_empty() && s.first.is_empty() {
            self.mark(false);
        }
        let next = s
            .due
            .iter()
            .chain(s.told.iter().map(|(p, (at, _))| (p, at)))
            .map(|(p, &at)| ready_at(&s, p, at))
            .fold(None, |m: Option<f64>, at| Some(m.map_or(at, |m| m.min(at))));
        (ready, next.map(|at| at - now))
    }

    /// A pass ended. One that failed (or never started) runs again a minute
    /// later, then two, up to an hour, looking at everything: no other event
    /// may come for it, and once one goes through its unit isn't failed any
    /// more. Returns when it runs again.
    pub fn done(&self, pass: &Pass, ok: bool, now: f64) -> Option<f64> {
        let mut s = self.lock();
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
        s.told.clear();
    }

    #[cfg(test)]
    pub fn told_now(&self) -> HashMap<Pass, (f64, BTreeSet<Told>)> {
        self.lock().told.clone()
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
    use crate::told::{First, Told};

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

    // A full pass sees to every repo whose event has come in, GAP or not.
    #[test]
    fn full_pass_first() {
        let line = Line::default();
        line.event(r("sam/r"), 100.0);
        took(&line, 105.0);
        line.event(r("sam/r"), 106.0);
        line.event(r("sam/q"), 106.0);
        line.event(Pass::Full, 106.0 - DEBOUNCE);
        line.wakes(&booked(json!({"sam/s": 500})));
        assert_eq!(took(&line, 111.0), (vec![Pass::Full], Some(389.0)));
        assert_eq!(line.due(), due(&[(r("sam/s"), 500.0)]));
        assert_eq!(took(&line, 120.0), (vec![], Some(380.0)));
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
    // booked, and one due now, the repo's or the full pass, takes them in. A
    // pass that failed runs again looking at everything.
    #[test]
    fn issues() {
        let line = Line::default();
        line.wakes(&booked(json!({"sam/r": 500})));
        line.told(r("sam/r"), vec![Told::Issue(7)], 100.0, false);
        line.told(r("Sam/R"), vec![Told::Issue(9)], 102.0, false);
        line.told(r("sam/q"), vec![Told::Issue(7)], 102.0, false);
        assert_eq!(line.take(104.0), (vec![], Some(1.0)));
        assert_eq!(
            line.take(105.0),
            (
                vec![(
                    r("sam/r"),
                    Scope::Told([Told::Issue(7), Told::Issue(9)].into())
                )],
                Some(2.0)
            )
        );
        assert_eq!(line.due(), due(&[(r("sam/r"), 500.0)]));
        assert_eq!(
            line.take(107.0).0,
            vec![(r("sam/q"), Scope::Told([Told::Issue(7)].into()))]
        );
        line.told(r("sam/r"), vec![Told::Issue(3)], 200.0, false);
        line.event(r("sam/r"), 200.0);
        assert_eq!(line.take(205.0), (vec![(r("sam/r"), Scope::All)], None));
        assert!(line.told_now().is_empty());
        line.told(r("sam/r"), vec![Told::Issue(4)], 300.0, false);
        line.event(Pass::Full, 300.0);
        assert_eq!(line.take(305.0), (vec![(Pass::Full, Scope::All)], None));
        assert!(line.told_now().is_empty());
        line.told(r("sam/r"), vec![Told::Issue(5)], 400.0, false);
        assert_eq!(
            line.take(405.0).0,
            vec![(r("sam/r"), Scope::Told([Told::Issue(5)].into()))]
        );
        line.done(&r("sam/r"), false, 406.0);
        assert_eq!(line.due(), due(&[(r("sam/r"), 466.0)]));
        assert!(line.told_now().is_empty());
    }

    fn firsts(repo: &str, n: u64, branch: &str) -> std::collections::BTreeMap<String, Vec<First>> {
        [(
            repo.to_owned(),
            vec![First {
                number: n,
                branch: Some(branch.into()),
                head: Some("abcdef1".into()),
            }],
        )]
        .into()
    }

    // An event about a PR that goes first starts its pass ahead of every
    // other due then, the full pass's too, which still runs after; one
    // about any other PR waits its turn among the rest.
    #[test]
    fn firsts_go_first() {
        let line = Line::default();
        line.firsts(firsts("sam/z", 4, "fix"));
        line.event(Pass::Full, 100.0 - DEBOUNCE);
        line.event(r("sam/a"), 100.0);
        assert!(!line.told(r("sam/b"), vec![Told::Pr(9)], 100.0, false));
        assert!(line.told(r("Sam/Z"), vec![Told::Branch("fix".into())], 100.0, false));
        assert_eq!(
            line.take(105.0).0,
            vec![(r("sam/z"), Scope::Told([Told::Branch("fix".into())].into()))]
        );
        assert_eq!(line.take(105.0).0, vec![(Pass::Full, Scope::All)]);
        // Its gap still holds: a first PR's pass is no faster than its own
        // last one allows.
        assert!(line.told(r("sam/z"), vec![Told::Pr(4)], 106.0, false));
        assert_eq!(line.take(111.0), (vec![], Some(14.0)));
        assert_eq!(
            line.take(125.0).0,
            vec![(r("sam/z"), Scope::Told([Told::Pr(4)].into()))]
        );
    }

    // An event that counts only for a first PR (a check run) books nothing
    // for any other, or before the passes have listed any.
    #[test]
    fn only_first() {
        let line = Line::default();
        assert!(!line.told(r("sam/z"), vec![Told::Pr(4)], 100.0, true));
        line.firsts(firsts("sam/z", 4, "fix"));
        assert!(!line.told(r("sam/z"), vec![Told::Pr(5)], 100.0, true));
        assert!(line.told_now().is_empty());
        assert!(line.told(
            r("sam/z"),
            vec![Told::Commit("ABCDEF1".into())],
            100.0,
            true
        ));
        assert_eq!(line.take(105.0).0.len(), 1);
    }

    // The marker is there from the event about a first PR until its pass
    // starts, and never for any other; one an earlier run left is gone.
    #[test]
    fn marker() {
        let dir = tempfile::tempdir().unwrap();
        let m = dir.path().join("autoland/.first");
        std::fs::create_dir_all(m.parent().unwrap()).unwrap();
        std::fs::write(&m, "").unwrap();
        let line = Line::default().with_marker(Some(m.clone()));
        assert!(!m.exists());
        line.firsts(firsts("sam/z", 4, "fix"));
        line.told(r("sam/b"), vec![Told::Pr(4)], 100.0, false);
        line.event(r("sam/z"), 100.0);
        assert!(!m.exists());
        line.told(r("sam/z"), vec![Told::Pr(4)], 100.0, false);
        assert!(m.exists());
        assert_eq!(line.take(104.0).0, vec![]);
        assert!(m.exists());
        assert_eq!(line.take(105.0).0, vec![(r("sam/z"), Scope::All)]);
        assert!(!m.exists());
        assert_eq!(
            line.take(105.0).0,
            vec![(r("sam/b"), Scope::Told([Told::Pr(4)].into()))]
        );
    }
}
