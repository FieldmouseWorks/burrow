//! burrow-review: the steward's reviewers, as a service of their own
//! (modules/autoland.nix runs it as `autoland-review`). It works through
//! the looks the steward's passes queue, a few at once per reviewer, and
//! leaves each answer for the repo's next pass to post.
//!
//! It runs as a user of its own with no GitHub login, and each agent runs
//! read-only in a sandbox that shows it its head's snapshot and little
//! else, with no network but its own API (egress.rs). So a PR that talks an
//! agent into reading, sending or saying something it shouldn't finds
//! nothing worth having: no GitHub token, no secrets paths, no other repo's
//! code, no way out but its API, and no key of its own (the commands Codex
//! runs can't read its login). The login's keys are taken out of anything
//! it says as well (scrub.rs).
//!
//! Each reviewer keeps one session per PR (sessions/REVIEWER--PR): the
//! agent's home, and the look it last took there, which its next look
//! resumes. Nothing in it is another PR's (only Codex's login is linked
//! in), and once the PR merges or closes the steward's sweep marks it gone
//! and this deletes it.
//!
//! What a look is asked is the steward's (its brief, in the queue entry);
//! that it's resuming a session, and where it left off, is this service's.

pub mod codex;
pub mod config;
pub mod egress;
pub mod queue;
pub mod scrub;
mod work;

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

pub use work::{forget, review, work};

use crate::config::Config;
use crate::egress::Dial;

/// Where the service says what it did: the journal.
pub type Log = Arc<dyn Fn(&str) + Send + Sync>;

pub struct Service {
    pub cfg: Config,
    /// This program, which a look's sandbox runs as its bridge.
    pub exe: PathBuf,
    /// The reviewers' user's home: its agents' logins and its git config.
    pub home: PathBuf,
    /// The PATH its agents run with.
    pub path: OsString,
    /// How the gate reaches an API's host.
    pub dial: Dial,
    pub log: Log,
    /// Held while a look puts a refreshed login back: looks run side by side.
    login: Mutex<()>,
    /// Every key the agents' logins have held while the service read them
    /// (scrub.rs): what a look says is scrubbed of the ones a refresh wrote
    /// while it ran, its own or a look's beside it, and of the ones those
    /// replaced.
    seen: Arc<Mutex<BTreeSet<String>>>,
}

impl Service {
    pub fn new(
        cfg: Config,
        exe: PathBuf,
        home: PathBuf,
        path: OsString,
        dial: Dial,
        log: Log,
    ) -> Service {
        Service {
            cfg,
            exe,
            home,
            path,
            dial,
            log,
            login: Mutex::new(()),
            seen: Arc::default(),
        }
    }

    fn say(&self, line: &str) {
        (self.log)(line);
    }

    /// The agents' keys: their logins' as they are now, and every one they
    /// have held since the service began, the longest first, so none is
    /// left half out by a shorter one inside it.
    fn keys(&self) -> Vec<String> {
        let mut seen = held(&self.seen);
        seen.extend(scrub::secrets(&self.home));
        let mut keys: Vec<String> = seen.iter().cloned().collect();
        keys.sort_by_key(|k| std::cmp::Reverse(k.len()));
        keys
    }

    /// The logins watched for each write to them (scrub.rs `Watch`), until
    /// what's returned is dropped; nothing to watch if there's no login.
    fn watch(&self) -> Option<scrub::Watch> {
        match scrub::Watch::start(&self.home, self.seen.clone()) {
            Ok(w) => Some(w),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => {
                self.say(&format!("couldn't watch the login for refreshes: {e}"));
                None
            }
        }
    }
}

/// A lock, held: one a panicking look left poisoned holds nothing broken.
fn held<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Why a look has no answer.
#[derive(Debug)]
pub enum Failed {
    /// The reviewer can't review anything for now (usage spent, not logged
    /// in): the steward leaves it out until `until`, and PRs wait for it.
    Out { why: String, until: f64 },
    /// This look failed: the steward tries again.
    Error(String),
}

/// Seconds since the epoch.
pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}
