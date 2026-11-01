//! A proof check that's a command and nothing more (a repo's `proofRuns`),
//! run by the autoland steward with no Claude job. The steward's pass
//! (modules/autoland/proofs.py, `start_run`) makes the run's folder, its
//! state dir's `builds/ID`, writes what the run is there (`build.json`,
//! [`Record`]) and has `burrow-proof start DIR` start its unit
//! ([`start`]), a transient one of the machine user's own:
//!
//! - `burrow-proof run DIR` is the unit's own process ([`run`]): each
//!   check's command in turn, in bash, in a fresh worktree of the repo's
//!   checkout at the PR's head, fenced in a cgroup of its own ([`fence`]).
//!   Its exit is the verdict ([`verdict`]).
//! - systemd bounds the whole of it, setup included (`RuntimeMaxSec`), and a
//!   stop or a timeout ends every process the unit has, the commands'
//!   included, however they hide (SIGTERM, then SIGKILL).
//! - `burrow-proof end DIR` is the unit's `ExecStopPost` ([`end`]), which
//!   runs however the unit ended: it removes the run's worktrees, says how
//!   it ended in `end.json` (what the steward reads) and kicks the repo's
//!   pass, which posts the verdicts and `comment.md`.
//!
//! A run proves its head under its commands, as a build proves its head's
//! derivation: nothing of the PR's base reaches it, so its verdict is the
//! commit's, whatever the PR targets.

pub mod comment;
pub mod end;
pub mod fence;
pub mod run;
pub mod start;
pub mod trees;

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// What a run is, as the steward writes it in its folder (`build.json`).
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    /// OWNER/REPO.
    pub repo: String,
    /// The PR's number: GitHub keeps its head as `refs/pull/N/head`.
    pub number: u64,
    /// The commit it runs on, all 40 hex digits.
    pub head: String,
    /// The repo's own clone on this machine (its projectsDir's).
    pub checkout: PathBuf,
    /// Each check's command, run in this order (by name).
    pub runs: BTreeMap<String, String>,
    /// The unit it runs in (`autoland-ID.service`).
    pub unit: String,
    /// Seconds the whole run may take before systemd ends it.
    pub timeout: u64,
    /// The shell a command runs in.
    pub bash: PathBuf,
    /// The git that makes and removes its worktrees.
    pub git: PathBuf,
    /// Where its worktrees go: `ID-I`, for its I-th check.
    pub trees: PathBuf,
    /// What starts the repo's pass once it's over (burrow-events' kick), if anything.
    pub kick: Option<String>,
    /// The most bytes its comment may hold.
    #[serde(rename = "commentBytes")]
    pub comment_bytes: usize,
}

/// The files in a run's folder besides `build.json`.
pub const LOG: &str = "log";
pub const CGROUP: &str = "cgroup";
pub const RAN: &str = "ran.json";
pub const COMMENT: &str = "comment.md";
pub const END: &str = "end.json";

/// The file the I-th check's command writes its summary to (PROOF_SUMMARY).
pub fn summary(dir: &Path, i: usize) -> PathBuf {
    dir.join(format!("summary-{i}.md"))
}

impl Record {
    /// `build.json` in DIR, read and held to what a run needs.
    pub fn read(dir: &Path) -> Result<Record, String> {
        let path = dir.join("build.json");
        let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let r: Record =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        r.check().map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(r)
    }

    fn check(&self) -> Result<(), String> {
        if self.repo.split('/').count() != 2 || self.repo.split('/').any(str::is_empty) {
            return Err(format!("repo {:?} isn't OWNER/REPO", self.repo));
        }
        if self.number == 0 {
            return Err("number 0 isn't a PR's".into());
        }
        if self.head.len() != 40 || !self.head.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!("head {:?} isn't a commit's full hash", self.head));
        }
        for (name, p) in [
            ("checkout", &self.checkout),
            ("bash", &self.bash),
            ("git", &self.git),
            ("trees", &self.trees),
        ] {
            if !p.is_absolute() {
                return Err(format!("{name} {} isn't an absolute path", p.display()));
            }
        }
        if self.runs.is_empty() {
            return Err("no runs".into());
        }
        let unit = self
            .unit
            .strip_prefix("autoland-")
            .and_then(|u| u.strip_suffix(".service"));
        if !unit.is_some_and(|u| !u.is_empty() && u.bytes().all(plain)) {
            return Err(format!("unit {:?} isn't autoland-ID.service", self.unit));
        }
        if self.timeout == 0 {
            return Err("timeout 0".into());
        }
        Ok(())
    }
}

/// A byte of a word systemd takes as it is in a unit's command line: no
/// quoting, no `$`, no `%`, no space.
pub fn plain(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"/._+@=,-".contains(&b)
}

/// A command's exit, as a verdict: `Some(true)` passed (0), `Some(false)`
/// failed (1 to 125); `None` is the machine failing, never a verdict: a
/// signal (128 and up, as a shell has it), a command not found or not
/// runnable (127, 126), or one no shell gives.
pub fn verdict(exit: i32) -> Option<bool> {
    match exit {
        0 => Some(true),
        1..=125 => Some(false),
        _ => None,
    }
}

/// How a run's checks came out, as `run` leaves it (`ran.json`) for `end`,
/// and as `end.json` keeps it: each check's verdict ([state, description],
/// as GitHub's status takes it) and its exit.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
pub struct Ran {
    pub verdicts: BTreeMap<String, [String; 2]>,
    pub exits: BTreeMap<String, i32>,
}

/// VALUE as JSON in PATH, whole or not at all (a reader never sees half).
pub fn save(path: &Path, value: &impl Serialize) -> io::Result<()> {
    let text = serde_json::to_string(value).map_err(io::Error::other)?;
    write_whole(path, text.as_bytes())
}

/// BYTES in PATH, whole or not at all.
pub fn write_whole(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests;
