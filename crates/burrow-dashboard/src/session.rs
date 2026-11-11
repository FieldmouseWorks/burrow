//! Sessions: a random token in a cookie, kept here only as its digest.
//! A session ends when it goes unused for the idle timeout, or at the hard
//! cap after it began, whichever comes first; using it moves only the first.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// How long a session may last (modules/dashboard.nix sets both).
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Seconds unused before it ends.
    pub idle: u64,
    /// Seconds after it began that it ends regardless.
    pub max: u64,
}

/// Using a session moves `seen` at once; the file catches up only when
/// it's this many seconds behind, so a page load isn't a disk write.
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

/// Every live session, by its token's digest.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Sessions(BTreeMap<String, Session>);

/// What `use_` found.
#[derive(Debug, PartialEq, Eq)]
pub enum Use {
    /// No such session, or it has ended (and is gone now).
    None,
    /// Live; `dirty` when the file should be written.
    Live { session: Session, dirty: bool },
}

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

    /// Use the session `key` at `now`: a live one is seen again, an ended
    /// one is dropped.
    pub fn use_(&mut self, key: &str, now: u64, limits: Limits) -> Use {
        let Some(s) = self.0.get_mut(key) else {
            return Use::None;
        };
        if !s.live(now, limits) {
            self.0.remove(key);
            return Use::None;
        }
        let dirty = now.saturating_sub(s.seen) >= SEEN_GRAIN;
        s.seen = s.seen.max(now);
        Use::Live {
            session: s.clone(),
            dirty,
        }
    }

    /// End the session `key`; whether there was one.
    pub fn end(&mut self, key: &str) -> bool {
        self.0.remove(key).is_some()
    }

    /// Drop every ended session; whether any went.
    pub fn sweep(&mut self, now: u64, limits: Limits) -> bool {
        let before = self.0.len();
        self.0.retain(|_, s| s.live(now, limits));
        self.0.len() != before
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: Limits = Limits {
        idle: 3600,
        max: 86400,
    };

    fn one(at: u64) -> Sessions {
        let mut s = Sessions::default();
        s.begin("k".into(), "Phone".into(), at);
        s
    }

    fn live(u: &Use) -> bool {
        matches!(u, Use::Live { .. })
    }

    #[test]
    fn an_unused_session_ends_at_the_idle_timeout() {
        let mut s = one(1000);
        assert!(live(&s.use_("k", 1000 + 3599, LIMITS)));
        let mut s2 = one(1000);
        assert_eq!(s2.use_("k", 1000 + 3600, LIMITS), Use::None);
        // And it's gone, not just refused.
        assert!(s2.is_empty());
        // Seen at 4599, it lasts until 8199.
        assert!(live(&s.use_("k", 8198, LIMITS)));
    }

    #[test]
    fn using_a_session_never_takes_it_past_the_hard_cap() {
        let mut s = one(0);
        // Seen every half hour, it never goes idle...
        let mut t = 0;
        while t + 1800 < 86400 {
            t += 1800;
            assert!(live(&s.use_("k", t, LIMITS)), "at {t}");
        }
        // ...and still ends a day after it began.
        assert_eq!(s.use_("k", 86400, LIMITS), Use::None);
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
        let mut s = one(5000);
        let Use::Live { session, .. } = s.use_("k", 4000, LIMITS) else {
            panic!("live");
        };
        assert_eq!(session.seen, 5000);
    }

    #[test]
    fn the_file_is_written_only_once_seen_moves_a_grain() {
        let mut s = one(0);
        assert_eq!(
            s.use_("k", SEEN_GRAIN - 1, LIMITS),
            Use::Live {
                session: Session {
                    began: 0,
                    seen: SEEN_GRAIN - 1,
                    passkey: "Phone".into()
                },
                dirty: false
            }
        );
        let Use::Live { dirty, .. } = s.use_("k", 2 * SEEN_GRAIN, LIMITS) else {
            panic!("live");
        };
        assert!(dirty);
    }

    #[test]
    fn ending_and_sweeping() {
        let mut s = one(0);
        s.begin("j".into(), "Laptop".into(), 3000);
        assert!(!s.sweep(3599, LIMITS));
        assert!(s.sweep(3600, LIMITS));
        assert_eq!(s.len(), 1);
        assert!(s.end("j"));
        assert!(!s.end("j"));
        assert_eq!(s.use_("j", 3001, LIMITS), Use::None);
    }

    #[test]
    fn an_unknown_token_is_no_session() {
        let mut s = one(0);
        assert_eq!(s.use_("nope", 1, LIMITS), Use::None);
        assert_eq!(s.len(), 1);
    }
}
