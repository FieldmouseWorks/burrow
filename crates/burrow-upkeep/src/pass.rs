//! One look at one repo, as a pass of the steward's has it: its time, the
//! GitHub it asks, its Claude jobs' states, its state files, and what it
//! says back to the pass (Out).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use burrow_github::{Api, Error, Response, call};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
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
/// cloned if need be, under its key, or lists `unstarted`. The part's next
/// look finds it by its key (Pass::asked_for), whatever became of its id.
#[derive(Debug, Serialize, PartialEq)]
pub struct Job {
    /// Whose it is: `dashboard` or `flaky`.
    pub part: &'static str,
    /// Routing role, required for every upkeep starter.
    pub role: &'static str,
    /// What proj keeps it under (`proj run --key`, one job a key): the
    /// part asks under the same key until a look finds a job under it, so
    /// a starter that outlived its pass can't start a second beside it.
    pub key: String,
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

/// The key a job a part asked for at `at` is kept under.
fn key(repo: &str, part: &str, at: f64) -> String {
    format!("upkeep/{repo}/{part}/{at}")
}

/// A job a part has asked for and no look has found yet.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Ask {
    /// The key it's asked for under, as it was given. It's kept whole, not
    /// made again from a time: a time read back from JSON can come out a
    /// hair off, and a key a hair off finds nothing and starts a second job.
    pub key: String,
    /// When it was last asked for: what a job found under the key is dated
    /// by, since one that took the key late started on a later ask.
    pub at: f64,
}

/// How a Claude job stands and what it holds open, read together (`proj
/// lifecycle`, burrow-proj's Job::lifecycle, under the job's turn-start
/// lock), so no turn starts between the two.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Lifecycle {
    /// queued, running, or how its latest turn ended (done, failed, timed
    /// out ...); None when nothing can say.
    pub state: Option<String>,
    /// What it holds open, a line each: `ask`, `owner`, `await TARGET`.
    pub holds: Vec<String>,
}

impl Lifecycle {
    /// Whether the job is still at work: queued, running, or nothing can
    /// say; or, however its turn ended, it holds something open, a question
    /// for a person or a PR or issue it awaits other than `own` (the PR it
    /// was handed: awaiting that is ending its turn), which proj resumes it
    /// on. The one word on whether a job ended, for every kind (the
    /// steward's Python reads the same, common.job_lifecycle): one that
    /// holds a question taken for ended gets a second job on the same work,
    /// and both carry on once the question's answered or times out
    /// (remi#881).
    pub fn at_work(&self, own: Option<&str>) -> bool {
        matches!(
            self.state.as_deref(),
            None | Some("queued" | "running" | "parked")
        ) || self.holds.iter().any(|h| match h.trim().split_once(' ') {
            Some(("await", target)) => own.is_none_or(|o| !target.trim().eq_ignore_ascii_case(o)),
            _ => !h.trim().is_empty(),
        })
    }

    /// Whether what it holds open includes `own`, the PR it was handed:
    /// its turn is over, but proj resumes it on that PR's next change. A
    /// fix job's claim follows it from turn to turn so (claims.rs).
    pub fn awaits(&self, own: &str) -> bool {
        self.holds.iter().any(|h| {
            matches!(h.trim().split_once(' '),
                Some(("await", target)) if target.trim().eq_ignore_ascii_case(own))
        })
    }
}

/// How a Claude job stands.
pub trait Jobs {
    /// Its lifecycle, as proj reads it at once.
    fn lifecycle(&mut self, jid: &str) -> Lifecycle;
    /// The job run under the key, once it has started (`proj find`).
    fn find(&mut self, key: &str) -> Result<Option<String>, String>;
}

/// proj's own word on a job (`proj lifecycle ID`). A status run takes every
/// job for running, as the steward's own does.
pub struct Proj {
    pub bin: PathBuf,
    pub dry: bool,
}

impl Jobs for Proj {
    /// Nothing can say (state None) when proj can't: it couldn't read the
    /// job's record, or isn't there to ask.
    fn lifecycle(&mut self, jid: &str) -> Lifecycle {
        if self.dry {
            return Lifecycle {
                state: Some("running".into()),
                holds: vec![],
            };
        }
        let Ok(out) = Command::new(&self.bin).args(["lifecycle", jid]).output() else {
            return Lifecycle::default();
        };
        let said = String::from_utf8_lossy(&out.stdout);
        let mut lines = said.lines().map(str::trim);
        match lines
            .next()
            .filter(|s| out.status.success() && !s.is_empty())
        {
            Some(state) => Lifecycle {
                state: Some(state.into()),
                holds: lines.map(String::from).collect(),
            },
            None => Lifecycle::default(),
        }
    }

    /// A status run's own jobs are all `dry`, as the steward's are.
    fn find(&mut self, key: &str) -> Result<Option<String>, String> {
        if self.dry {
            return Ok(Some("dry".into()));
        }
        let out = Command::new(&self.bin)
            .args(["find", key])
            .output()
            .map_err(|e| format!("{}: {e}", self.bin.display()))?;
        if !out.status.success() {
            let why = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(format!("proj find: {} {why}", out.status));
        }
        let said = String::from_utf8_lossy(&out.stdout).trim().to_string();
        Ok((!said.is_empty()).then_some(said))
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

    /// Ask for a part's job now: under the key of the ask that stands, or a
    /// new one if none does. What it gives is the key to ask under.
    pub fn ask(&self, part: &str, stands: &mut Option<Ask>) -> String {
        let key = match stands.take() {
            Some(ask) => ask.key,
            None => key(self.repo, part, self.now),
        };
        *stands = Some(Ask {
            key: key.clone(),
            at: self.now,
        });
        key
    }

    /// The job a part asked for, as proj has it under the ask's key: None
    /// while none has started.
    pub fn asked_for(&mut self, ask: &Ask) -> Result<Option<String>, String> {
        let key = &ask.key;
        self.jobs
            .find(key)
            .map_err(|e| format!("whether the job it asked for started ({key}): {e}"))
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
        load(&self.state_dir(), name)
    }

    /// A state file, written whole or not at all. A status run that can't
    /// write the state dir (from a shell of someone else's) is fine.
    pub fn save<T: Serialize>(&self, name: &str, value: &T) -> Result<(), String> {
        match save(&self.state_dir(), name, value) {
            Err(_) if self.dry => Ok(()),
            done => done,
        }
    }

    /// What GitHub has at `path`, or what went wrong (Pass::noted).
    pub fn get(&mut self, path: &str) -> Result<Value, Error> {
        call(self.api, "GET", path, None, None)
            .and_then(|r| r.json())
            .map_err(|e| self.noted(e))
    }

    /// Every page of a listing from `path` on, as one list, or what went
    /// wrong (Pass::noted).
    pub fn pages(&mut self, path: &str) -> Result<Vec<Value>, Error> {
        let mut all = vec![];
        let mut next = Some(path.to_string());
        while let Some(at) = next {
            let r: Response = call(self.api, "GET", &at, None, None).map_err(|e| self.noted(e))?;
            match r.json().map_err(|e| self.noted(e))? {
                Value::Array(page) => all.extend(page),
                _ => {
                    return Err(Error::said(format!(
                        "GET {at}: GitHub's answer isn't a list"
                    )));
                }
            }
            next = r.next(&at);
        }
        Ok(all)
    }

    /// GitHub's error, once a rate limit it met is noted: the pass may ask
    /// again no earlier than the error's own deadline (`retry_at`, which
    /// burrow-github sets for every rate limit it recognizes: a 403's, a
    /// 429's, GraphQL's), nor than the reset of a budget `rate_limit` says
    /// is spent, nor than the probe's own deadline when `rate_limit` is
    /// itself limited (it can be: a secondary limit covers it too). That
    /// probe costs nothing against the primary limit, and runs once a
    /// pass; a failed, malformed or healthy one can't shorten the error's
    /// deadline, and a later limit's deadline only lengthens the pass's.
    pub fn noted(&mut self, e: Error) -> Error {
        if let Some(retry_at) = e.retry_at {
            let mut at = retry_at as f64;
            if self.out.limited.is_none() {
                let probe =
                    match call(self.api, "GET", "rate_limit", None, None).and_then(|r| r.json()) {
                        Ok(v) => reset(&v),
                        Err(limited) => limited.retry_at.map_or(0.0, |t| t as f64),
                    };
                at = at.max(probe);
            }
            self.out.limited = Some(self.out.limited.map_or(at, |was| was.max(at)));
        }
        e
    }

    /// What GitHub's error says, once noted.
    pub fn github(&mut self, e: Error) -> String {
        self.noted(e).why
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
        format!("{}[bot]", self.cfg.app.slug)
    }
}

/// The repo's own state file: what settle, the branch watch and its hook
/// keep, each its own part of it, beside what the steward keeps there
/// itself.
pub fn repo_file(repo: &str) -> String {
    format!("repo-{}.json", repo.replace('/', "_"))
}

/// The state file `name` in `dir`, or its default when there's none or it
/// can't be read.
pub fn load<T: DeserializeOwned + Default>(dir: &Path, name: &str) -> T {
    fs::read_to_string(dir.join(name))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// The state file `name` in `dir`, written whole or not at all.
pub fn save<T: Serialize>(dir: &Path, name: &str, value: &T) -> Result<(), String> {
    let path = dir.join(name);
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    fs::write(&tmp, text)
        .and_then(|()| fs::rename(&tmp, &path))
        .map_err(|e| format!("{}: {e}", path.display()))
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_parked_job_still_owns_its_maintenance_brief() {
        let life = Lifecycle {
            state: Some("parked".into()),
            holds: Vec::new(),
        };
        assert!(life.at_work(None));
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

    mod limits {
        use super::super::Pass;
        use crate::testing::{Hub, States, T, answer, config, look};
        use burrow_github::{call, query};
        use serde_json::json;

        const SECONDARY: &str = "You have exceeded a secondary rate limit";

        fn healthy(hub: &mut Hub) {
            let budgets = json!({"resources": {"core": {"remaining": 4000, "reset": T + 3000.0}}});
            hub.on("GET", "rate_limit$", answer(200, budgets));
        }

        fn noted(hub: &mut Hub, f: impl FnOnce(&mut Pass)) -> Option<f64> {
            let d = tempfile::tempdir().unwrap();
            let cfg = config(d.path());
            let mut jobs = States::new("running");
            let (out, _) = look(&cfg, hub, &mut jobs, 0.0, |p| {
                f(p);
                Ok(())
            });
            out.limited
        }

        fn get(p: &mut Pass, path: &str) {
            p.get(path).unwrap_err();
        }

        #[test]
        fn a_secondary_limit_keeps_its_retry_after_past_a_healthy_probe() {
            let mut hub = Hub::default();
            let no = answer(403, json!({"message": SECONDARY})).with("Retry-After", "600");
            hub.on("GET", "repos/sam/r$", no);
            healthy(&mut hub);
            let at = noted(&mut hub, |p| get(p, "repos/sam/r"));
            assert_eq!(at, Some(T + 600.0));
        }

        #[test]
        fn a_primary_403_keeps_its_reset() {
            let mut hub = Hub::default();
            let reset = format!("{}", T as u64 + 1200);
            let no = answer(403, json!({"message": "Forbidden"}))
                .with("x-ratelimit-remaining", "0")
                .with("x-ratelimit-reset", &reset);
            hub.on("GET", "repos/sam/r$", no);
            // The probe fails: it can't erase the error's own deadline.
            hub.on("GET", "rate_limit$", answer(502, json!(null)));
            let at = noted(&mut hub, |p| get(p, "repos/sam/r"));
            assert_eq!(at, Some(T + 1200.0));
        }

        #[test]
        fn a_429_with_no_words_of_a_limit_keeps_its_retry_after() {
            let mut hub = Hub::default();
            let no = answer(429, json!({"message": "Too many"})).with("Retry-After", "300");
            hub.on("GET", "repos/sam/r$", no);
            // The probe can't answer at all: still the error's deadline.
            let mut gone = answer(0, json!(null));
            gone.why = "gh: connection refused".into();
            hub.on("GET", "rate_limit$", gone);
            let at = noted(&mut hub, |p| get(p, "repos/sam/r"));
            assert_eq!(at, Some(T + 300.0));
        }

        #[test]
        fn a_graphql_quota_error_is_noted() {
            let mut hub = Hub::default();
            let errors = json!({"errors": [{"type": "RATE_LIMITED", "message": "quota spent"}]});
            let no = answer(200, errors).with("Retry-After", "900");
            hub.graphql.push(("viewer".into(), no));
            healthy(&mut hub);
            let at = noted(&mut hub, |p| {
                let e = query(p.api, "sam", "query { viewer { login } }", json!({}));
                p.noted(e.unwrap_err());
            });
            assert_eq!(at, Some(T + 900.0));
        }

        #[test]
        fn a_spent_budget_that_resets_later_wins() {
            let mut hub = Hub::default();
            let no = answer(403, json!({"message": SECONDARY})).with("Retry-After", "60");
            hub.on("GET", "repos/sam/r$", no);
            let spent = json!({"resources": {"core": {"remaining": 0, "reset": T + 2400.0}}});
            hub.on("GET", "rate_limit$", answer(200, spent));
            let at = noted(&mut hub, |p| get(p, "repos/sam/r"));
            assert_eq!(at, Some(T + 2400.0));
        }

        #[test]
        fn a_later_limit_only_lengthens_the_pass_s_wait() {
            let mut hub = Hub::default();
            let soon = answer(403, json!({"message": SECONDARY})).with("Retry-After", "60");
            let late = answer(403, json!({"message": SECONDARY})).with("Retry-After", "900");
            hub.on("GET", "repos/sam/a$", soon.clone());
            hub.on("GET", "repos/sam/b$", late);
            hub.on("GET", "repos/sam/c$", soon);
            healthy(&mut hub);
            let at = noted(&mut hub, |p| {
                for path in ["repos/sam/a", "repos/sam/b", "repos/sam/c"] {
                    get(p, path);
                }
            });
            assert_eq!(at, Some(T + 900.0));
        }

        /// What `rate_limit` answers, after a secondary limit of a minute
        /// on `repos/sam/r`: the pass's deadline.
        fn probed(probe: crate::testing::Answer) -> Option<f64> {
            let mut hub = Hub::default();
            let no = answer(403, json!({"message": SECONDARY})).with("Retry-After", "60");
            hub.on("GET", "repos/sam/r$", no);
            hub.on("GET", "rate_limit$", probe);
            noted(&mut hub, |p| get(p, "repos/sam/r"))
        }

        #[test]
        fn a_limited_probe_s_own_longer_deadline_wins() {
            let secondary = answer(403, json!({"message": SECONDARY})).with("Retry-After", "600");
            assert_eq!(probed(secondary), Some(T + 600.0));
            let reset = format!("{}", T as u64 + 1200);
            let primary = answer(403, json!({"message": "Forbidden"}))
                .with("x-ratelimit-remaining", "0")
                .with("x-ratelimit-reset", &reset);
            assert_eq!(probed(primary), Some(T + 1200.0));
            let wordless = answer(429, json!({"message": "Too many"})).with("Retry-After", "900");
            assert_eq!(probed(wordless), Some(T + 900.0));
        }

        #[test]
        fn a_probe_that_says_nothing_of_use_keeps_the_error_s_deadline() {
            // A limited probe whose deadline is sooner, and one whose
            // answer isn't the budgets at all.
            let sooner = answer(429, json!({"message": "Too many"})).with("Retry-After", "10");
            assert_eq!(probed(sooner), Some(T + 60.0));
            assert_eq!(probed(answer(200, json!([1, 2]))), Some(T + 60.0));
        }

        #[test]
        fn a_limited_probe_runs_once_a_pass_and_its_deadline_holds() {
            let mut hub = Hub::default();
            let soon = answer(403, json!({"message": SECONDARY})).with("Retry-After", "60");
            hub.on("GET", "repos/sam/a$", soon.clone());
            hub.on("GET", "repos/sam/b$", soon);
            let probe = answer(403, json!({"message": SECONDARY})).with("Retry-After", "600");
            hub.on("GET", "rate_limit$", probe);
            let at = noted(&mut hub, |p| {
                get(p, "repos/sam/a");
                get(p, "repos/sam/b");
            });
            assert_eq!(at, Some(T + 600.0));
            assert_eq!(
                hub.paths().iter().filter(|p| **p == "rate_limit").count(),
                1
            );
        }

        #[test]
        fn what_isn_t_a_limit_notes_nothing() {
            let mut hub = Hub::default();
            hub.on(
                "GET",
                "repos/sam/r$",
                answer(404, json!({"message": "Not Found"})),
            );
            healthy(&mut hub);
            let at = noted(&mut hub, |p| {
                call(p.api, "GET", "repos/sam/r", None, None)
                    .map_err(|e| p.noted(e))
                    .unwrap_err();
            });
            assert_eq!(at, None);
        }
    }
}
