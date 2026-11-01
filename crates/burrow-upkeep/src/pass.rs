//! One look at one repo, as a pass of the steward's has it: its time, the
//! GitHub it asks, its Claude jobs' states, its state files, and what it
//! says back to the pass (Out).

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use burrow_github::{Api, Error, call};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::config::Config;

pub const MIN: f64 = 60.0;
pub const HOUR: f64 = 3600.0;
pub const DAY: f64 = 86400.0;

/// A line for the steward's lists, as its digest has them: what kind of
/// thing it is (the heading it's listed under), where (a repo, an issue),
/// and what about it.
pub type Line = (String, String, String);

/// What a look says back to the pass, as JSON on stdout.
#[derive(Debug, Default, Serialize, PartialEq)]
pub struct Out {
    /// For the digest: the owner's issue or the pipeline's stuck one, as
    /// the steward sorts them by heading.
    pub stuck: Vec<Line>,
    /// When the repo's next pass is due, for what this look waits on.
    pub wake: Option<f64>,
    /// What of the look failed, a line each (`its dashboard: why`): the
    /// pass logs each, and the stall watchdog watches the repo.
    pub failed: Vec<String>,
    /// When the rate limit a call met resets (0 when GitHub won't say),
    /// once one has.
    pub limited: Option<f64>,
    /// Whether it searched for the repo's dashboard, which the pass counts
    /// against its searches (dashboard::FINDS).
    pub searched: bool,
    /// The Claude jobs it asks the pass to start.
    pub jobs: Vec<Job>,
}

/// A Claude job a look asks for: the pass starts it in the repo's project,
/// cloned if need be, and says so (`started`), or lists `unstarted`.
#[derive(Debug, Serialize, PartialEq)]
pub struct Job {
    /// Whose it is: `dashboard` or `flaky`, the part `started` tells.
    pub part: &'static str,
    pub task: String,
    /// Whether it's the pipeline's work, which goes ahead of the line, or
    /// the project's.
    pub pipeline: bool,
    /// How long its turns may run, in minutes.
    pub timeout: u64,
    /// When the repo's next pass is due once it has started: its timeout,
    /// for a kick that's lost when it ends.
    pub wake: f64,
    /// What's listed when it can't start.
    pub unstarted: Vec<Line>,
}

/// How a Claude job stands.
pub trait Jobs {
    /// queued, running, or how it ended (done, failed, timed out ...); None
    /// when nothing can say.
    fn state(&mut self, jid: &str) -> Option<String>;
}

/// proj's own word on a job (`proj state ID`). A status run takes every
/// job for running, as the steward's own does.
pub struct Proj {
    pub bin: PathBuf,
    pub dry: bool,
}

impl Jobs for Proj {
    fn state(&mut self, jid: &str) -> Option<String> {
        if self.dry {
            return Some("running".into());
        }
        let out = Command::new(&self.bin).args(["state", jid]).output().ok()?;
        let said = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!said.is_empty()).then_some(said)
    }
}

pub struct Pass<'a> {
    pub cfg: &'a Config,
    pub repo: &'a str,
    /// When the steward's pass started: every time this look books or
    /// keeps is reckoned from it.
    pub now: f64,
    /// A status run's: its own state, and nothing it can't write fails it.
    pub dry: bool,
    /// How many repos this pass has searched for a dashboard before this one.
    pub searched: usize,
    pub api: &'a mut dyn Api,
    pub jobs: &'a mut dyn Jobs,
    pub out: Out,
}

impl Pass<'_> {
    /// Ask for the repo's next pass at `at`, if that's still to come.
    pub fn wake(&mut self, at: f64) {
        if at > self.now {
            self.out.wake = Some(self.out.wake.map_or(at, |w| w.min(at)));
        }
    }

    pub fn list(&mut self, heading: &str, at: String, what: String) {
        self.out.stuck.push((heading.to_string(), at, what));
    }

    pub fn state_dir(&self) -> PathBuf {
        if self.dry {
            self.cfg.state_dir.join("dry")
        } else {
            self.cfg.state_dir.clone()
        }
    }

    /// A state file, or its default when there's none or it can't be read.
    pub fn load<T: DeserializeOwned + Default>(&self, name: &str) -> T {
        fs::read_to_string(self.state_dir().join(name))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    /// A state file, written whole or not at all. A status run that can't
    /// write the state dir (from a shell of someone else's) is fine.
    pub fn save<T: Serialize>(&self, name: &str, value: &T) -> Result<(), String> {
        let path = self.state_dir().join(name);
        let tmp = path.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
        match fs::write(&tmp, text).and_then(|()| fs::rename(&tmp, &path)) {
            Ok(()) => Ok(()),
            Err(_) if self.dry => Ok(()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// What GitHub's error says, once a rate limit it met is noted: when it
    /// resets, from `rate_limit`, which costs nothing against it. A limit
    /// with none of it spent (a secondary one) resets at 0, which is past.
    pub fn github(&mut self, e: Error) -> String {
        if self.out.limited.is_none() && e.0.to_lowercase().contains("rate limit") {
            let reset = call(self.api, "GET", "rate_limit", None, None)
                .ok()
                .and_then(|r| r.json().ok())
                .map_or(0.0, |v| reset(&v));
            self.out.limited = Some(reset);
        }
        e.0
    }

    /// The steward's authors, as REST gives a login: one of `authors`, or
    /// of `bots` as SLUG[bot], a name no person's can be.
    pub fn ours(&self, login: &str) -> bool {
        match login.strip_suffix("[bot]") {
            Some(slug) => self.cfg.bots.iter().any(|b| b == slug),
            None => !login.is_empty() && self.cfg.authors.iter().any(|a| a == login),
        }
    }

    /// The steward's own login, as REST gives it: its App's bot.
    pub fn me(&self) -> String {
        format!("{}[bot]", self.cfg.app)
    }
}

/// When the rate limits GitHub's `rate_limit` says are spent reset, the
/// last of them; 0 for none.
fn reset(v: &Value) -> f64 {
    v["resources"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(_, r)| r["remaining"].as_i64() == Some(0))
        .filter_map(|(_, r)| r["reset"].as_f64())
        .fold(0.0, f64::max)
}

/// Whether GitHub's error is it not answering (a rate limit, a 5xx, no
/// connection) rather than saying no: the same look goes again next pass,
/// and counts against nothing.
pub fn unavailable(why: &str) -> bool {
    let w = why.to_lowercase();
    let http = w
        .match_indices("http ")
        .any(|(i, _)| match w.as_bytes().get(i + 5..i + 8) {
            Some([b'4', b'2', b'9']) => true,
            Some([b'5', a, b]) => a.is_ascii_digit() && b.is_ascii_digit(),
            _ => false,
        });
    let eof = w.match_indices("eof").any(|(i, _)| {
        let word = |b: Option<&u8>| b.is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_');
        !word(i.checked_sub(1).and_then(|j| w.as_bytes().get(j))) && !word(w.as_bytes().get(i + 3))
    });
    http || eof
        || [
            "rate limit",
            "error connecting",
            "connection reset",
            "connection refused",
            "timeout",
            "time out",
            "timedout",
            "timed out",
            "network is unreachable",
            "no such host",
            "tls handshake",
        ]
        .iter()
        .any(|s| w.contains(s))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn what_github_not_answering_looks_like() {
        for why in [
            "GET repos/sam/r/issues: HTTP 503, 3 times",
            "GET repos/sam/r/issues: HTTP 403: API rate limit exceeded for user",
            "GET a: HTTP 429, 3 times",
            "gh: error connecting to api.github.com",
            "gh: read: connection reset by peer",
            "gh: Post \"https://api.github.com/graphql\": EOF",
            "gh: i/o timeout",
            "gh: net/http: TLS handshake timeout",
        ] {
            assert!(unavailable(why), "{why}");
        }
        for why in [
            "GET repos/sam/r/issues: HTTP 404: Not Found",
            "GET a: HTTP 403: Resource not accessible",
            "GET a: HTTP 50: what",
            "geofence",
            "flaky: \"sam/.r\" isn't OWNER/REPO",
        ] {
            assert!(!unavailable(why), "{why}");
        }
    }

    #[test]
    fn a_rate_limit_resets_when_its_last_spent_one_does() {
        let v = json!({"resources": {"core": {"remaining": 0, "reset": 1800000100},
                                     "search": {"remaining": 0, "reset": 1800000050},
                                     "graphql": {"remaining": 10, "reset": 1800009999}}});
        assert_eq!(reset(&v), 1800000100.0);
        assert_eq!(
            reset(&json!({"resources": {"core": {"remaining": 5, "reset": 1}}})),
            0.0
        );
        assert_eq!(reset(&json!({})), 0.0);
    }
}
