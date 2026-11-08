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
        if let Some(repo) = only {
            return self.one(repo);
        }
        let Some(app) = &self.app else {
            return self.renovate("", &[]);
        };
        // Every repo the installations reach, a run each. One that fails
        // doesn't hold up the rest, but fails the pass.
        let mut failed = 0;
        for owner in &app.owners {
            let Some(repos) = self.reached(owner) else {
                complain(&format!(
                    "couldn't list the repos {owner}'s installation reaches"
                ));
                failed = 1;
                continue;
            };
            for repo in repos.split_whitespace() {
                if self.one(repo) != 0 {
                    complain(&format!("{repo}: Renovate's run failed"));
                    failed = 1;
                }
            }
        }
        failed
    }

    /// One repo. Renovate's own code applies its autodiscoverFilter here, to
    /// the repo as GitHub spells it (burrow-events has it in lower case, and
    /// a /regex/ filter can mind case). Then its own autodiscovery, narrowed
    /// to this repo, applies the rest (topics, forks, archived, mirrors).
    fn one(&self, repo: &str) -> u8 {
        let mut gh = self.github();
        let Some(repo) = burrow_github::call(&mut gh, "GET", &format!("repos/{repo}"), None, None)
            .map_err(|e| complain(&e.to_string()))
            .ok()
            .and_then(|r| r.json().ok())
            .and_then(|v| v["full_name"].as_str().map(str::to_owned))
        else {
            return 1;
        };
        match self.command(&self.tools.covers).arg(&repo).status() {
            Ok(s) if s.success() => {}
            Ok(s) if s.code() == Some(10) => {
                say(&format!(
                    "{repo}: not one of Renovate's (autodiscoverFilter)"
                ));
                return 0;
            }
            _ => {
                complain(&format!("{repo}: couldn't apply autodiscoverFilter"));
                return 1;
            }
        }
        let filter = serde_json::Value::from(vec![repo.as_str()]);
        self.renovate(&repo, &[format!("--autodiscover-filter={filter}")])
    }

    /// The repos `owner`'s installation reaches, archived ones aside.
    fn reached(&self, owner: &str) -> Option<String> {
        let mut gh = self.github();
        gh.env.push(("GH_TOKEN_OWNER".into(), owner.into()));
        let pages = burrow_github::pages(&mut gh, "installation/repositories").ok()?;
        let mut repos = Vec::new();
        for page in pages {
            for repo in page["repositories"].as_array()? {
                if repo["archived"] != true {
                    repos.push(repo["full_name"].as_str()?.to_owned());
                }
            }
        }
        Some(repos.join("\n"))
    }

    /// Renovate with `args` on `repo` (every repo, as the user, when it's
    /// empty), in its turn, with a token asked for once it's its turn.
    fn renovate(&self, repo: &str, args: &[String]) -> u8 {
        let lock = match OpenOptions::new()
            .append(true)
            .create(true)
            .open(&self.run_lock)
        {
            Ok(lock) => lock,
            Err(e) => return self.broke(&self.run_lock, e),
        };
        if let Err(e) = flock(&lock, FlockOperation::LockExclusive) {
            return self.broke(&self.run_lock, e.into());
        }
        // Renovate holds it too, until it's gone: a renovate-run killed
        // while its Renovate still runs (or shuts down) leaves no gap for
        // another run on the same clones.
        if let Err(e) = fcntl_setfd(&lock, FdFlags::empty()) {
            return self.broke(&self.run_lock, e.into());
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
            Ok(out) => return exit_of(out.status),
            Err(e) => {
                complain(&format!(
                    "{}: {}: {e}",
                    self.name,
                    asked.get_program().display()
                ));
                return NOT_RUN;
            }
        };
        let ran = self
            .command(&self.tools.renovate)
            .args(args)
            .env("RENOVATE_TOKEN", &token)
            .env("GITHUB_COM_TOKEN", &token)
            .status();
        let rc = match ran {
            Ok(status) => exit_of(status),
            Err(e) => {
                complain(&format!(
                    "{}: {}: {e}",
                    self.name,
                    self.tools.renovate.display()
                ));
                NOT_RUN
            }
        };
        match self.ran(if repo.is_empty() { "all" } else { repo }) {
            Ok(()) => rc,
            Err((path, e)) => self.broke(&path, e),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn its_config_is_every_setting_and_only_those() {
        let config = serde_json::json!({
            "name": "renovate-run", "runLock": "/r/run.lock", "runs": "/r/runs",
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
