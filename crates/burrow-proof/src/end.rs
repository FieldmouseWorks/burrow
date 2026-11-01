//! `burrow-proof end DIR`, the run's unit's `ExecStopPost`: systemd runs it
//! however the unit ended (done, failed, stopped, past its RuntimeMaxSec),
//! once it has ended every other process of the unit. Every fence left is
//! cleared and every worktree of the run removed; then `end.json` says how
//! it ended, as the steward reads a proof's end (modules/autoland/proofs.py
//! `verdicts`, common.py `build_state`):
//!
//! - `done`, with each check's verdict and exit, only when the run's own
//!   process exited 0 and left `ran.json`;
//! - `timed out` when the unit's result is `timeout`: past its
//!   RuntimeMaxSec, or a stop that SIGTERM didn't end in time;
//! - `failed` otherwise: stopped, killed, or ended short (run.rs).
//!
//! Neither of the last two is a verdict: the steward runs it again.
//!
//! Then the repo's pass is kicked, which posts what it says.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::{CGROUP, END, LOG, RAN, Ran, Record, fence, run, save, trees};

/// How the unit ended, as systemd tells an ExecStopPost.
#[derive(Debug, Clone, Default)]
pub struct Ended {
    /// SERVICE_RESULT: `success`, `timeout`, `exit-code`, `signal`...
    pub result: String,
    /// EXIT_CODE: `exited`, `killed`, `dumped`.
    pub code: String,
    /// EXIT_STATUS: its exit status, or its signal's name.
    pub status: String,
}

impl Ended {
    pub fn from_env() -> Ended {
        let var = |k| std::env::var(k).unwrap_or_default();
        Ended {
            result: var("SERVICE_RESULT"),
            code: var("EXIT_CODE"),
            status: var("EXIT_STATUS"),
        }
    }
}

/// The run whose folder is DIR, ended as HOW says: what's left of it
/// cleared, `end.json` written, its repo kicked. What `end.json` says, or
/// why it couldn't be written.
pub fn end(dir: &Path, how: &Ended) -> Result<Value, String> {
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(LOG));
    let note = |text: &str| {
        if let Ok(mut log) = log.as_ref() {
            let _ = writeln!(log, "autoland: {text}");
        }
        eprintln!("burrow-proof: {}: {text}", dir.display());
    };
    let rec = Record::read(dir);
    let id = run::id(dir)?;
    // Each fence emptied before the worktrees go: one that won't empty
    // keeps them, and the next sweep leaves them while its cgroup is there.
    let cleared = match fs::read_to_string(dir.join(CGROUP)) {
        Ok(at) => fence::clear_all(Path::new(at.trim_end())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    };
    match (&rec, cleared) {
        (Ok(rec), Ok(())) => {
            for said in own_trees(rec, id) {
                note(&said);
            }
        }
        (_, Err(e)) => note(&format!(
            "can't clear its fences, so its worktrees stay: {e}"
        )),
        (Err(e), Ok(())) => note(&format!("can't tell where its worktrees are: {e}")),
    }
    let ran: Option<Ran> = fs::read_to_string(dir.join(RAN))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok());
    let at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    let exit: Option<i32> = (how.code == "exited")
        .then(|| how.status.parse().ok())
        .flatten();
    let said = match ran {
        Some(ran) if how.result == "success" && exit == Some(0) => json!({
            "state": "done", "exit": 0, "verdicts": ran.verdicts, "exits": ran.exits, "at": at,
        }),
        _ if how.result == "timeout" => json!({"state": "timed out", "exit": null, "at": at}),
        _ => json!({"state": "failed", "exit": exit, "at": at}),
    };
    note(&format!(
        "ended {} ({} {} {})",
        said["state"].as_str().unwrap_or_default(),
        how.result,
        how.code,
        how.status
    ));
    save(&dir.join(END), &said).map_err(|e| format!("{}: {e}", dir.join(END).display()))?;
    if let Ok(Some(url)) = rec.as_ref().map(|r| r.kick.as_deref())
        && let Err(e) = kick(url)
    {
        note(&format!("couldn't kick {url}: {e}"));
    }
    Ok(said)
}

/// Every worktree of the run (`ID-I` under its trees) removed; what
/// couldn't be, in words.
fn own_trees(rec: &Record, id: &str) -> Vec<String> {
    let prefix = format!("{id}-");
    let Ok(entries) = fs::read_dir(&rec.trees) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
        .filter_map(|e| {
            trees::remove(&rec.git, &rec.checkout, &e.path())
                .err()
                .map(|err| format!("can't remove {}: {err}", e.path().display()))
        })
        .collect()
}

/// The pass URL names, started now (burrow-events' kick). Best effort: the
/// steward's hourly pass is there for one that doesn't get through.
pub fn kick(url: &str) -> Result<(), String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(5)))
        .build()
        .into();
    agent
        .post(url)
        .send_empty()
        .map(drop)
        .map_err(|e| e.to_string())
}
