//! claude-rc-scan: which Remote Control servers run (modules/remote-control.nix
//! has the units), and which stale ones restart once no session holds them.
//!
//!   claude-rc-scan             a scan (claude-rc-scan.service)
//!   claude-rc-scan done        what it runs as it stops: asks for a look after
//!   claude-rc-scan again ID    that look (claude-rc-again@ID.service)
//!
//! - decide.rs: the decisions, with no I/O: what makes a project, which
//!   sessions hold a server, which server is current, when the wake comes
//! - machine.rs: what they read: the folders, the sessions, the processes,
//!   and the units it starts and stops
//! - scan.rs: the scan, `done` and `again`
//!
//! A start while a scan runs (a path unit's, the booked timer's) only joins
//! it, until it has stopped, so what changed meanwhile would go unseen: a
//! session's status, a folder added or gone, a checkout's origin written (a
//! clone writes it just after making the folder), a quiet spell's end. So
//! each scan notes what it saw as it began and when its spells end, in
//! `$XDG_RUNTIME_DIR/claude-rc-scan/INVOCATION_ID.{seen,wake}`, and as it
//! stops (`done`) asks for a look after it (claude-rc-again@ID, `again ID`),
//! which waits for it to be stopped and scans again if anything changed
//! since or a spell is over. That look has a unit of its own, so no start
//! joins it; and its start comes once this scan's is over, so nothing is
//! lost after it either: a scan already running then has a look of its own
//! to come.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

pub mod decide;
pub mod machine;
pub mod scan;

/// What modules/remote-control.nix tells claude-rc-scan about the machine.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Config {
    /// burrow.projectsDir: a server for each project in it.
    pub projects_dir: PathBuf,
    /// The machine's own flake (burrow.flake): served whatever it is.
    pub flake: PathBuf,
    /// How long a stale server waits on a session that has stopped doing
    /// anything.
    pub idle_minutes: u64,
    /// The tmux and the claude-rc-serve a current server runs: a server
    /// whose command line isn't `TMUX ... SERVE...` is stale.
    pub tmux: PathBuf,
    pub serve: PathBuf,
    pub tools: Tools,
}

/// What it runs, by path: a unit's Exec has systemd's bare PATH.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Tools {
    pub systemctl: PathBuf,
    pub systemd_run: PathBuf,
    pub systemd_escape: PathBuf,
    pub git: PathBuf,
    /// Reads a session's file, once for each thing it wants of it.
    pub jq: PathBuf,
}

impl Config {
    pub fn read(path: &Path) -> Result<Config, Broke> {
        let text =
            fs::read_to_string(path).map_err(|e| Broke(format!("read {}: {e}", path.display())))?;
        serde_json::from_str(&text).map_err(|e| Broke(format!("{}: {e}", path.display())))
    }
}

/// Why a scan stopped.
#[derive(Debug)]
pub struct Broke(pub String);

impl fmt::Display for Broke {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<std::io::Error> for Broke {
    fn from(e: std::io::Error) -> Broke {
        Broke(e.to_string())
    }
}
