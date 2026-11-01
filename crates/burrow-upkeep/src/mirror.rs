//! The steward's bare clone of each repo, which only it reads, and git as
//! the steward runs it. Refs are fetched into it from GitHub: what the
//! reviewers' snapshots are taken from (snapshot.rs), and what a proof's
//! builds and a merge's effect are worked out from (proofs.py, prs.py
//! `onto`).

use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::config::Config;

/// git with its arguments and environment, run from `git()` or `git_in()`.
pub struct Git {
    cmd: Command,
    input: Option<Vec<u8>>,
    /// Its first two arguments, which say what failed.
    said: Vec<String>,
}

/// git, from the word's own environment.
pub fn git() -> Git {
    Git {
        cmd: Command::new("git"),
        input: None,
        said: vec![],
    }
}

/// git on the repo at `dir`.
pub fn git_in(dir: &Path) -> Git {
    git().arg("-C").arg(dir)
}

impl Git {
    pub fn arg(mut self, a: impl AsRef<OsStr>) -> Git {
        if self.said.len() < 2 {
            self.said.push(a.as_ref().to_string_lossy().into_owned());
        }
        self.cmd.arg(a);
        self
    }

    pub fn args<S: AsRef<OsStr>>(self, args: impl IntoIterator<Item = S>) -> Git {
        args.into_iter().fold(self, Git::arg)
    }

    pub fn env(mut self, k: &str, v: impl AsRef<OsStr>) -> Git {
        self.cmd.env(k, v);
        self
    }

    /// What it reads on stdin.
    pub fn input(mut self, bytes: Vec<u8>) -> Git {
        self.input = Some(bytes);
        self
    }

    /// What it printed, or why it failed: its first two arguments, and the
    /// start of what it said.
    pub fn run(mut self) -> Result<Vec<u8>, String> {
        let what = format!("git {}", self.said.join(" "));
        let stdin = if self.input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        };
        let mut child = self
            .cmd
            .stdin(stdin)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("{what}: {e}"))?;
        if let (Some(bytes), Some(mut pipe)) = (self.input.take(), child.stdin.take()) {
            // All it's handed is a list of paths, which it reads whole
            // before it says anything.
            pipe.write_all(&bytes).map_err(|e| format!("{what}: {e}"))?;
        }
        let out = child
            .wait_with_output()
            .map_err(|e| format!("{what}: {e}"))?;
        if out.status.success() {
            return Ok(out.stdout);
        }
        let said: String = String::from_utf8_lossy(&out.stderr)
            .trim()
            .chars()
            .take(300)
            .collect();
        Err(format!("{what}: {said}"))
    }
}

/// Where the repo's mirror is, in the steward's state.
pub fn path(state: &Path, repo: &str) -> PathBuf {
    state
        .join("review/git")
        .join(format!("{}.git", repo.replace('/', "__")))
}

/// Whether a refspec is one of refs: anything else (an option, say) isn't
/// fetched.
pub fn refspec(r: &str) -> bool {
    r.strip_prefix('+').unwrap_or(r).starts_with("refs/")
}

/// The repo's mirror, with `refs` fetched into it from GitHub, as the token
/// in GH_TOKEN (gh is git's only credential helper). Made the first time.
pub fn fetch(cfg: &Config, state: &Path, repo: &str, refs: &[String]) -> Result<PathBuf, String> {
    let mirror = path(state, repo);
    if let Some(r) = refs.iter().find(|r| !refspec(r)) {
        return Err(format!("{r:?} isn't a refspec of refs"));
    }
    if !mirror.is_dir() {
        git().args(["init", "-q", "--bare"]).arg(&mirror).run()?;
    }
    let url: OsString = format!("{}/{repo}.git", cfg.git.trim_end_matches('/')).into();
    git_in(&mirror)
        .args([
            "-c",
            "credential.helper=",
            "-c",
            "credential.helper=!gh auth git-credential",
        ])
        .args(["fetch", "-q", "--no-tags"])
        .arg(url)
        .args(refs)
        .run()?;
    Ok(mirror)
}
