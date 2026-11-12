//! renovate-run: Renovate over every repo it covers, or over one.
//!
//! As the machine's own App, a token is one owner's installation's and good
//! for fifty minutes from when it's asked for (gh-token), so each repo gets
//! a run of its own with its own token, and a full one is every repo the
//! installations of the App's owners reach. As the user, one run with gh's
//! login covers them all. Runs take the run lock in turn: they share the
//! clones. A run's token is asked for once it has the lock, so its fifty
//! minutes aren't spent waiting on another's. Each run that ends, failed or
//! not, is noted in `runs`.
//!
//! GitHub's rate limit is a wait, not a failure: the installation's hourly
//! allowance is shared with everything else acting as the App. A repo it
//! holds off, and the rest of its owner's in a full run, is booked again in
//! the renovate handler's wake file (burrow-events) for when it lifts, as is
//! the full pass when an owner's repos can't be listed; the pass exits 0
//! unless something else failed. Renovate meets the limit too, mid-run (it
//! stops with "rate-limit-exceeded"): a run of Renovate's that fails while
//! GitHub's `rate_limit`, asked with the very token it ran with, says that
//! allowance is spent is booked the same way, not failed. A run that never
//! got Renovate going (its lock, its token) or couldn't be noted fails
//! whatever the allowance.
//!
//! It runs as the user; what it runs, it runs by path (`Config::tools`).

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use rustix::fs::{FlockOperation, flock};
use rustix::io::{FdFlags, fcntl_setfd};
use serde::Deserialize;

use crate::{NOT_RUN, complain, exit_of, say, trimmed};

/// What modules/updates.nix tells renovate-run.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Config {
    /// renovate-run, as the machine names it: how it says itself.
    pub name: String,
    /// What runs take in turn.
    pub run_lock: PathBuf,
    /// When each repo's runs ended (`all` for one over every repo), the
    /// last ten, for autoland's dashboard watcher.
    pub runs: PathBuf,
    /// The renovate handler's wake file (burrow-events), {repo: epoch} with
    /// "" for the full pass: what GitHub's rate limit held off is booked
    /// there for when it lifts.
    pub wake: PathBuf,
    /// The machine's own App, when Renovate runs as it rather than as the
    /// user.
    pub app: Option<App>,
    pub tools: Tools,
}

/// The App Renovate acts as.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct App {
    /// The env a process acts as the App in (burrow.github.apps.NAME.env),
    /// for everything it runs.
    pub env: BTreeMap<String, String>,
    /// The owners whose installations a full run covers, a run per repo.
    pub owners: Vec<String>,
    /// What hands out the App's tokens.
    pub gh_token: PathBuf,
}

/// What it runs, by path.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Tools {
    /// gh, as the App when there's one.
    pub gh: PathBuf,
    pub renovate: PathBuf,
    /// Whether Renovate's autodiscoverFilter takes a repo, as its own code
    /// has it (updates/renovate-covers.mjs): exit 0 if it does, 10 if not.
    pub covers: PathBuf,
}

impl Config {
    pub fn read(path: &Path) -> Result<Config, String> {
        let text = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_slice(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Renovate over `only`, or over every repo: what it exits with.
    pub fn run(&self, only: Option<&str>) -> u8 {
        let started = now();
        if let Some(repo) = only {
            return match self.one(repo) {
                Ok(rc) => rc.max(self.used_up(repo, started)),
                Err(held) => self.held(repo, &held),
            };
        }
        let Some(app) = &self.app else {
            let rc = match self.renovate("", &[]) {
                Err(rc) | Ok((rc @ 0, _)) => rc,
                Ok((rc, token)) => self.spent(token).map_or(rc, |held| self.held("", &held)),
            };
            return rc.max(self.used_up("", started));
        };
        // Every repo the installations reach, a run each. One that fails
        // doesn't hold up the rest, but fails the pass. Each owner's
        // installation has a rate limit of its own: one that holds off a
        // repo holds off the rest of that owner's, which aren't asked about.
        let mut failed = 0;
        for owner in &app.owners {
            let repos = match self.reached(owner) {
                Ok(repos) => repos,
                Err(e) => {
                    failed = failed.max(match e.retry_at {
                        Some(_) => self.held("", &e),
                        None => {
                            complain(&format!(
                                "{e}\ncouldn't list the repos {owner}'s installation reaches"
                            ));
                            1
                        }
                    });
                    continue;
                }
            };
            let mut limited: Option<u64> = None;
            for repo in &repos {
                if let Some(at) = limited {
                    failed = failed.max(self.book(repo, at, None));
                    continue;
                }
                match self.one(repo) {
                    Ok(0) => {}
                    Ok(_) => {
                        complain(&format!("{repo}: Renovate's run failed"));
                        failed = 1;
                    }
                    Err(held) => {
                        limited = held.retry_at;
                        failed = failed.max(self.held(repo, &held));
                    }
                }
            }
        }
        failed.max(self.used_up("", started))
    }

    /// One repo, or the GitHub rate limit that held it off. Renovate's own
    /// code applies its autodiscoverFilter here, to the repo as GitHub
    /// spells it (burrow-events has it in lower case, and a /regex/ filter
    /// can mind case). Then its own autodiscovery, narrowed to this repo,
    /// applies the rest (topics, forks, archived, mirrors).
    fn one(&self, repo: &str) -> Result<u8, burrow_github::Error> {
        let mut gh = self.github();
        let asked = match burrow_github::call(&mut gh, "GET", &format!("repos/{repo}"), None, None)
        {
            Err(e) if e.retry_at.is_some() => return Err(e),
            asked => asked,
        };
        let Some(repo) = asked
            .map_err(|e| complain(&e.to_string()))
            .ok()
            .and_then(|r| r.json().ok())
            .and_then(|v| v["full_name"].as_str().map(str::to_owned))
        else {
            return Ok(1);
        };
        match self.command(&self.tools.covers).arg(&repo).status() {
            Ok(s) if s.success() => {}
            Ok(s) if s.code() == Some(10) => {
                say(&format!(
                    "{repo}: not one of Renovate's (autodiscoverFilter)"
                ));
                return Ok(0);
            }
            _ => {
                complain(&format!("{repo}: couldn't apply autodiscoverFilter"));
                return Ok(1);
            }
        }
        let filter = serde_json::Value::from(vec![repo.as_str()]);
        match self.renovate(&repo, &[format!("--autodiscover-filter={filter}")]) {
            Err(rc) | Ok((rc @ 0, _)) => Ok(rc),
            Ok((rc, token)) => self.spent(token).map_or(Ok(rc), Err),
        }
    }

    /// After a run of Renovate's failed: GitHub's rate limit, as an error
    /// that holds the run off, when `rate_limit` (which costs nothing
    /// against it) says the allowance of `token`, the one Renovate ran with,
    /// is spent: its installation's as the App, the user's login's as the
    /// user, whatever repo or checkout is around. None when it isn't, or
    /// GitHub won't say: then the run failed.
    fn spent(&self, token: String) -> Option<burrow_github::Error> {
        let mut gh = burrow_github::Gh {
            token: Some(token),
            ..self.github()
        };
        let budget = burrow_github::call(&mut gh, "GET", "rate_limit", None, None)
            .ok()?
            .json()
            .ok()?;
        let at = now();
        let mut meter = burrow_github::quota::Meter::new("", "", at);
        meter.budget(&budget);
        let lifts = meter.until(at)?;
        Some(burrow_github::Error {
            why: "Renovate's run met GitHub's rate limit".into(),
            unanswered: true,
            retry_at: Some(lifts),
        })
    }

    /// The repos `owner`'s installation reaches, archived ones aside.
    fn reached(&self, owner: &str) -> Result<Vec<String>, burrow_github::Error> {
        let mut gh = self.github();
        gh.env.push(("GH_TOKEN_OWNER".into(), owner.into()));
        let pages = burrow_github::pages(&mut gh, "installation/repositories")?;
        let odd = || {
            burrow_github::Error::said(
                "GET installation/repositories: an answer that isn't a list of repos",
            )
        };
        let mut repos = Vec::new();
        for page in pages {
            for repo in page["repositories"].as_array().ok_or_else(odd)? {
                if repo["archived"] != true {
                    repos.push(repo["full_name"].as_str().ok_or_else(odd)?.to_owned());
                }
            }
        }
        Ok(repos)
    }

    /// `key`'s pass (a repo, or "" for every repo) held off by GitHub's rate
    /// limit, as `held` says: booked again for when it lifts.
    fn held(&self, key: &str, held: &burrow_github::Error) -> u8 {
        let at = held.retry_at.unwrap_or_else(|| now() + 60);
        self.book(key, at, Some(held))
    }

    /// Book `key`'s pass again for `at`, saying so (and what GitHub said,
    /// when it was asked): 0, or 1 when it can't be booked.
    fn book(&self, key: &str, at: u64, said: Option<&burrow_github::Error>) -> u8 {
        if let Err(e) = burrow_github::deferred::book(&self.wake, key, at) {
            return self.broke(&self.wake, io::Error::other(e));
        }
        let what = if key.is_empty() { "every repo" } else { key };
        let when = i64::try_from(at)
            .ok()
            .and_then(|at| jiff::Timestamp::from_second(at).ok())
            .map_or_else(|| at.to_string(), |at| at.to_string());
        let mut line = format!("{what}: booked again for {when}, when GitHub's rate limit lifts");
        if let Some(said) = said {
            line = format!("{line}: {said}");
        }
        say(&line);
        0
    }

    /// `key`'s pass, started at `started`, ran: a booking it was started
    /// for is used up.
    fn used_up(&self, key: &str, started: u64) -> u8 {
        match burrow_github::deferred::ran(&self.wake, key, started) {
            Ok(()) => 0,
            Err(e) => self.broke(&self.wake, io::Error::other(e)),
        }
    }

    /// Renovate with `args` on `repo` (every repo, as the user, when it's
    /// empty), in its turn, with a token asked for once it's its turn.
    /// Renovate's exit and the token it ran with, once its run is noted; Err
    /// with the exit when the runner failed (its lock, its token, starting
    /// Renovate, noting the run), which no rate limit excuses.
    fn renovate(&self, repo: &str, args: &[String]) -> Result<(u8, String), u8> {
        let lock = match OpenOptions::new()
            .append(true)
            .create(true)
            .open(&self.run_lock)
        {
            Ok(lock) => lock,
            Err(e) => return Err(self.broke(&self.run_lock, e)),
        };
        if let Err(e) = flock(&lock, FlockOperation::LockExclusive) {
            return Err(self.broke(&self.run_lock, e.into()));
        }
        // Renovate holds it too, until it's gone: a renovate-run killed
        // while its Renovate still runs (or shuts down) leaves no gap for
        // another run on the same clones.
        if let Err(e) = fcntl_setfd(&lock, FdFlags::empty()) {
            return Err(self.broke(&self.run_lock, e.into()));
        }
        let mut asked = match &self.app {
            Some(app) => {
                let mut c = self.command(&app.gh_token);
                c.args(["get", repo.split('/').next().unwrap_or_default()]);
                c
            }
            None => {
                let mut c = self.command(&self.tools.gh);
                c.args(["auth", "token"]);
                c
            }
        };
        let token = match asked.stderr(Stdio::inherit()).output() {
            Ok(out) if out.status.success() => trimmed(out.stdout),
            Ok(out) => return Err(exit_of(out.status)),
            Err(e) => {
                complain(&format!(
                    "{}: {}: {e}",
                    self.name,
                    asked.get_program().display()
                ));
                return Err(NOT_RUN);
            }
        };
        let ran = self
            .command(&self.tools.renovate)
            .args(args)
            .env("RENOVATE_TOKEN", &token)
            .env("GITHUB_COM_TOKEN", &token)
            .status();
        let rc = match ran {
            Ok(status) => Ok(exit_of(status)),
            Err(e) => {
                complain(&format!(
                    "{}: {}: {e}",
                    self.name,
                    self.tools.renovate.display()
                ));
                Err(NOT_RUN)
            }
        };
        match self.ran(if repo.is_empty() { "all" } else { repo }) {
            Ok(()) => rc.map(|rc| (rc, token)),
            Err((path, e)) => Err(self.broke(&path, e)),
        }
    }

    /// When a run on `name` ended: the last ten, in a file named for the
    /// repo in lower case with its / as _, for autoland's dashboard watcher,
    /// which hands on an error still on a repo's dashboard after two of
    /// them.
    fn ran(&self, name: &str) -> Result<(), (PathBuf, io::Error)> {
        let file = self.runs.join(name.to_ascii_lowercase().replace('/', "_"));
        fs::create_dir_all(&self.runs).map_err(|e| (self.runs.clone(), e))?;
        let kept = fs::read(&file).unwrap_or_default();
        let lines: Vec<&[u8]> = kept
            .split(|&b| b == b'\n')
            .filter(|l| !l.is_empty())
            .collect();
        let mut text = Vec::new();
        for line in &lines[lines.len().saturating_sub(9)..] {
            text.extend_from_slice(line);
            text.push(b'\n');
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        text.extend_from_slice(format!("{now}\n").as_bytes());
        let mut tmp = OsString::from(file.as_os_str());
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        fs::write(&tmp, text).map_err(|e| (tmp.clone(), e))?;
        fs::rename(&tmp, &file).map_err(|e| (file, e))
    }

    fn github(&self) -> burrow_github::Gh {
        burrow_github::Gh {
            program: Some(self.tools.gh.clone().into_os_string()),
            env: self
                .app
                .as_ref()
                .map(|a| a.env.iter().map(|(k, v)| (k.into(), v.into())).collect())
                .unwrap_or_default(),
            ..Default::default()
        }
    }

    /// `program`, as the App when there's one.
    fn command(&self, program: &Path) -> Command {
        let mut c = Command::new(program);
        c.stdin(Stdio::null());
        if let Some(app) = &self.app {
            c.envs(&app.env);
        }
        c
    }

    fn broke(&self, path: &Path, e: io::Error) -> u8 {
        complain(&format!("{}: {}: {e}", self.name, path.display()));
        1
    }
}

/// The time now, in seconds since the epoch, as wake files have it.
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn its_config_is_every_setting_and_only_those() {
        let config = serde_json::json!({
            "name": "renovate-run", "runLock": "/r/run.lock", "runs": "/r/runs", "wake": "/r/wake.json",
            "app": {"env": {"GH_TOKEN_SOCKET": "/run/gh-token/w.sock"}, "owners": ["sam"], "ghToken": "/b/gh-token"},
            "tools": {"gh": "/b/gh", "renovate": "/b/renovate", "covers": "/b/covers"},
        });
        let cfg: Config = serde_json::from_value(config.clone()).unwrap();
        let app = cfg.app.unwrap();
        assert_eq!(app.owners, ["sam"]);
        assert_eq!(app.env["GH_TOKEN_SOCKET"], "/run/gh-token/w.sock");
        let mut user = config.clone();
        user["app"] = serde_json::Value::Null;
        assert!(
            serde_json::from_value::<Config>(user)
                .unwrap()
                .app
                .is_none()
        );
        let mut more = config.clone();
        more["stray"] = 1.into();
        assert!(serde_json::from_value::<Config>(more).is_err());
        let mut less = config;
        less["app"].as_object_mut().unwrap().remove("ghToken");
        assert!(serde_json::from_value::<Config>(less).is_err());
    }
}
