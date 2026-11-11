//! Sessions: a random token in a cookie, kept here only as its digest.
//! A session ends when it goes unused for the idle timeout, or at the hard
//! cap after it began, whichever comes first; using it moves only the first.
//!
//! What's saved ([`Sessions`]) says when each session was last used as of
//! its last save; [`Activity`] holds the uses since, in memory, so a page
//! load isn't a disk write. A use is saved once it's `SEEN_GRAIN` past
//! what's saved, measured from the save, so however often a session is used
//! the file is never more than a grain behind.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

/// How long a session may last (modules/dashboard.nix sets both).
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Seconds unused before it ends.
    pub idle: u64,
    /// Seconds after it began that it ends regardless.
    pub max: u64,
}

/// Seconds the saved last use may lag behind the real one.
pub const SEEN_GRAIN: u64 = 60;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub began: u64,
    pub seen: u64,
    /// The name of the passkey that began it.
    pub passkey: String,
}

impl Session {
    /// When it ends if it isn't used again.
    pub fn ends(&self, limits: Limits) -> u64 {
        self.began
            .saturating_add(limits.max)
            .min(self.seen.saturating_add(limits.idle))
    }

    pub fn live(&self, now: u64, limits: Limits) -> bool {
        now < self.ends(limits)
    }
}

/// Every session as saved, by its token's digest.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Sessions(BTreeMap<String, Session>);

impl Sessions {
    /// Begin a session; its token's digest is `key`.
    pub fn begin(&mut self, key: String, passkey: String, now: u64) {
        self.0.insert(
            key,
            Session {
                began: now,
                seen: now,
                passkey,
            },
        );
    }

    pub fn get(&self, key: &str) -> Option<&Session> {
        self.0.get(key)
    }

    /// Record a use of `key` at `seen`, never moving it back.
    pub fn touch(&mut self, key: &str, seen: u64) {
        if let Some(s) = self.0.get_mut(key) {
            s.seen = s.seen.max(seen);
        }
    }

    /// End the session `key`; whether there was one.
    pub fn end(&mut self, key: &str) -> bool {
        self.0.remove(key).is_some()
    }

    /// Drop every ended session.
    pub fn sweep(&mut self, now: u64, limits: Limits) {
        self.0.retain(|_, s| s.live(now, limits));
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Uses not yet saved, by session.
#[derive(Debug, Default)]
pub struct Activity(HashMap<String, u64>);

/// What using a session found.
#[derive(Debug, PartialEq, Eq)]
pub enum Use {
    /// No such session, or it has ended.
    None,
    /// Live, as it now stands; `save` when its use has to be saved now.
    Live { session: Session, save: bool },
}

impl Activity {
    /// Use the session `key` at `now`. Its use is held here until it's
    /// saved (`saved`), so a save that fails is tried again on the next.
    pub fn use_(&mut self, saved: &Sessions, key: &str, now: u64, limits: Limits) -> Use {
        let Some(s) = saved.get(key) else {
            self.0.remove(key);
            return Use::None;
        };
        let mut session = s.clone();
        session.seen = session.seen.max(self.0.get(key).copied().unwrap_or(0));
        if !session.live(now, limits) {
            self.0.remove(key);
            return Use::None;
        }
        session.seen = session.seen.max(now);
        self.0.insert(key.to_owned(), session.seen);
        let save = session.seen - s.seen >= SEEN_GRAIN;
        Use::Live { session, save }
    }

    /// The use of `key` is saved.
    pub fn saved(&mut self, key: &str) {
        self.0.remove(key);
    }

    /// Every use not saved yet, into `sessions`, to be saved with them.
    pub fn fold(&self, sessions: &mut Sessions) {
        for (key, seen) in &self.0 {
            sessions.touch(key, *seen);
        }
    }

    /// Every use is saved.
    pub fn clear(&mut self) {
        self.0.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: Limits = Limits {
        idle: 3600,
        max: 86400,
    };

    /// One session, `k`, begun at `at`.
    fn one(at: u64) -> (Sessions, Activity) {
        let mut s = Sessions::default();
        s.begin("k".into(), "Phone".into(), at);
        (s, Activity::default())
    }

    /// Use `k` at `now`, saving when asked to: whether it's live.
    fn use_(s: &mut Sessions, a: &mut Activity, now: u64) -> bool {
        match a.use_(s, "k", now, LIMITS) {
            Use::None => false,
            Use::Live { session, save } => {
                if save {
                    s.touch("k", session.seen);
                    a.saved("k");
                }
                true
            }
        }
    }

    #[test]
    fn an_unused_session_ends_at_the_idle_timeout() {
        let (mut s, mut a) = one(1000);
        assert!(use_(&mut s, &mut a, 1000 + 3599));
        // Used at 4599, it lasts until 8199.
        assert!(use_(&mut s, &mut a, 8198));
        assert!(!use_(&mut s, &mut a, 8198 + 3600));
        let (mut s, mut a) = one(1000);
        assert!(!use_(&mut s, &mut a, 1000 + 3600));
    }

    #[test]
    fn using_a_session_never_takes_it_past_the_hard_cap() {
        let (mut s, mut a) = one(0);
        // Used every half hour, it never goes idle...
        let mut t = 0;
        while t + 1800 < 86400 {
            t += 1800;
            assert!(use_(&mut s, &mut a, t), "at {t}");
        }
        // ...and still ends a day after it began.
        assert!(!use_(&mut s, &mut a, 86400));
    }

    #[test]
    fn the_end_is_whichever_comes_first() {
        let s = Session {
            began: 0,
            seen: 86000,
            passkey: String::new(),
        };
        assert_eq!(s.ends(LIMITS), 86400);
        let s = Session {
            began: 0,
            seen: 100,
            passkey: String::new(),
        };
        assert_eq!(s.ends(LIMITS), 3700);
    }

    #[test]
    fn a_clock_that_steps_back_doesnt_rewind_seen() {
        let (s, mut a) = one(5000);
        let Use::Live { session, .. } = a.use_(&s, "k", 4000, LIMITS) else {
            panic!("live");
        };
        assert_eq!(session.seen, 5000);
    }

    #[test]
    fn frequent_uses_are_saved_a_grain_after_the_last_save() {
        let (mut s, mut a) = one(0);
        // Used every half grain for an hour: what's saved is never more
        // than a grain behind.
        let mut t = 0;
        while t < 3600 {
            t += SEEN_GRAIN / 2;
            assert!(use_(&mut s, &mut a, t));
            let saved = s.get("k").unwrap().seen;
            assert!(t - saved < SEEN_GRAIN, "at {t}, saved {saved}");
        }
        // So after a restart (no activity) it lives on from the last save.
        let mut fresh = Activity::default();
        assert!(matches!(
            fresh.use_(&s, "k", t + 3600 - SEEN_GRAIN, LIMITS),
            Use::Live { .. }
        ));
    }

    #[test]
    fn a_use_whose_save_failed_is_saved_on_the_next() {
        let (s, mut a) = one(0);
        // Asked to save, and the save fails: s stays as it was.
        assert!(matches!(
            a.use_(&s, "k", SEEN_GRAIN, LIMITS),
            Use::Live { save: true, .. }
        ));
        // The next use still asks, and the session still counts the use.
        let Use::Live { session, save } = a.use_(&s, "k", SEEN_GRAIN + 1, LIMITS) else {
            panic!("live");
        };
        assert!(save);
        assert_eq!(session.seen, SEEN_GRAIN + 1);
        // Held in memory, the failed use keeps the session alive past the
        // saved idle limit.
        assert!(matches!(
            a.use_(&s, "k", 3600 + 1, LIMITS),
            Use::Live { .. }
        ));
    }

    #[test]
    fn ending_and_sweeping() {
        let (mut s, mut a) = one(0);
        s.begin("j".into(), "Laptop".into(), 3000);
        s.sweep(3599, LIMITS);
        assert_eq!(s.len(), 2);
        s.sweep(3600, LIMITS);
        assert_eq!(s.len(), 1);
        assert!(s.end("j"));
        assert!(!s.end("j"));
        assert_eq!(a.use_(&s, "j", 3001, LIMITS), Use::None);
    }

    #[test]
    fn an_unknown_token_is_no_session() {
        let (s, mut a) = one(0);
        assert_eq!(a.use_(&s, "nope", 1, LIMITS), Use::None);
    }
}
