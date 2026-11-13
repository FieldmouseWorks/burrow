//! The steward's bare clone of each repo, which only it reads, and git as
//! the steward runs it. Refs are fetched into it from GitHub: what the
//! reviewers' snapshots are taken from (snapshot.rs), and what a proof's
//! builds and a merge's effect are worked out from (proofs.py, prs.py
//! `onto`).

use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

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
    pub fn run(self) -> Result<Vec<u8>, String> {
        let (what, out) = self.output()?;
        if out.status.success() {
            return Ok(out.stdout);
        }
        Err(failed(&what, &out))
    }

    /// What it printed, None if it exited 1 (a merge that conflicts, say),
    /// or why it failed otherwise.
    pub fn run_or_1(self) -> Result<Option<Vec<u8>>, String> {
        let (what, out) = self.output()?;
        match out.status.code() {
            Some(0) => Ok(Some(out.stdout)),
            Some(1) => Ok(None),
            _ => Err(failed(&what, &out)),
        }
    }

    /// What it said of itself and how it exited, with what to call it.
    fn output(mut self) -> Result<(String, Output), String> {
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
        Ok((what, out))
    }
}

/// Why git failed: what it was, and the start of what it said.
fn failed(what: &str, out: &Output) -> String {
    let said: String = String::from_utf8_lossy(&out.stderr)
        .trim()
        .chars()
        .take(300)
        .collect();
    format!("{what}: {said}")
}

/// Where the repo's mirror is, in the steward's state.
pub fn path(state: &Path, repo: &str) -> PathBuf {
    state
        .join("review/git")
        .join(format!("{}.git", repo.replace('/', "__")))
}

/// Whether a refspec is one of refs, or a commit's id into one of refs:
/// anything else (an option, say) isn't fetched.
pub fn refspec(r: &str) -> bool {
    let r = r.strip_prefix('+').unwrap_or(r);
    r.starts_with("refs/")
        || r.split_once(':')
            .is_some_and(|(src, dst)| id(src) && dst.starts_with("refs/"))
}

/// Whether `s` is a commit's full id, as git writes it.
pub fn id(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The repo's mirror, with PR `n`'s head fetched into refs/pr/N, and
/// `more` beside it. GitHub keeps a PR's head as refs/pull/N/head, but can
/// lose that ref while the PR stays open (it did, on remi#495, and a close
/// and reopen didn't bring it back): then it's fetched by the id of `head`,
/// the head the PR reports, and refs/pr/N is that commit or the fetch
/// fails. With the ref there, refs/pr/N is where it points, which a push
/// since may have moved past `head`.
pub fn fetch_pr(
    cfg: &Config,
    state: &Path,
    repo: &str,
    n: u64,
    head: &str,
    more: &[String],
) -> Result<PathBuf, String> {
    let into = format!("refs/pr/{n}");
    let refs = |src: &str| {
        let mut refs = vec![format!("+{src}:{into}")];
        refs.extend_from_slice(more);
        refs
    };
    let pull = format!("refs/pull/{n}/head");
    let lost = match fetch(cfg, state, repo, &refs(&pull)) {
        Err(why) if why.contains(&format!("couldn't find remote ref {pull}")) => why,
        got => return got,
    };
    if !id(head) {
        return Err(format!(
            "{lost}; and {head:?} isn't a commit's id to fetch instead"
        ));
    }
    let m = fetch(cfg, state, repo, &refs(head))
        .map_err(|e| format!("{lost}; and by its head's id: {e}"))?;
    let got = git_in(&m)
        .args(["rev-parse", "--verify", "-q", &format!("{into}^{{commit}}")])
        .run()
        .map(|out| String::from_utf8_lossy(&out).trim().to_string())
        .unwrap_or_default();
    if got != head {
        return Err(format!(
            "{lost}; and {head} fetched by its id isn't a commit"
        ));
    }
    Ok(m)
}

/// How long, in seconds, a fetch may hear nothing from GitHub before git
/// gives it up. A pass waits on its fetches with no deadline of its own
/// (a big repo's first one takes as long as it takes), so a connection
/// that goes quiet would otherwise hold the pass until its unit's deadline
/// kills it, as one did on 2026-11-11 (burrow#634). Under a second byte a
/// second is nothing at all: the keepalives GitHub sends while it packs
/// keep a slow fetch going.
pub const STALL: u64 = 120;

/// The repo's mirror, with `refs` fetched into it from GitHub, as the token
/// in GH_TOKEN (gh is git's only credential helper). Made the first time.
/// It fails once GitHub has said nothing for STALL seconds.
pub fn fetch(cfg: &Config, state: &Path, repo: &str, refs: &[String]) -> Result<PathBuf, String> {
    fetch_within(cfg, state, repo, refs, STALL)
}

/// `fetch`, given up after `stall` seconds of silence.
fn fetch_within(
    cfg: &Config,
    state: &Path,
    repo: &str,
    refs: &[String],
    stall: u64,
) -> Result<PathBuf, String> {
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
            "-c",
            "http.lowSpeedLimit=1",
            "-c",
        ])
        .arg(format!("http.lowSpeedTime={stall}"))
        .args(["fetch", "-q", "--no-tags"])
        .arg(url)
        .args(refs)
        .run()?;
    Ok(mirror)
}

#[cfg(test)]
#[path = "mirror_tests.rs"]
mod tests;
