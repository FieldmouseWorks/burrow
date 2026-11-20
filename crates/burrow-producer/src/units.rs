//! A producer's work run as a unit of its own (`UNIT@ID.service`), as a
//! user systemd makes for it, which the module lets the producer's user
//! alone start and stop (polkit): the proof producer's runs and the judge
//! producer's looks. The unit keeps how its worker exited until it's
//! stopped, and the producer reads that, and whatever the worker left in
//! the folder only the producer and that worker can reach.
use crate::serve::Work;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// How a unit ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
    /// Its worker exited with this, as systemd kept it.
    Exited(u8),
    /// It ended some other way (killed, timed out, failed to start): no
    /// verdict, nothing ready.
    Broke(String),
}

/// The worker's units.
pub trait Units: Send + Sync {
    /// Start `id`'s unit and wait for it to end: how it did. An error
    /// when the wait was abandoned or ran out first; the unit is then still
    /// the caller's to stop.
    fn run(&self, id: &str, work: &Work, until: Instant) -> Result<Ended, String>;
    /// Stop `id`'s unit, which lets go of it.
    fn stop(&self, id: &str) -> Result<(), String>;
    /// The ids whose units are loaded now.
    fn loaded(&self) -> Result<Vec<String>, String>;
}

/// A new id for a unit, which names it and so the user systemd makes for
/// it (`proof-ID`, `judge-ID`, one a unit): 20 hex digits, as a user's
/// name is at most 31 characters, where a record's id is 32.
pub fn run_id() -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    format!(
        "{now:016x}{:04x}",
        N.fetch_add(1, Ordering::SeqCst) & 0xffff
    )
}

/// The worker's units, through systemctl: `UNIT@ID.service`, which the
/// module lets the producer's user start and stop and nothing else. The
/// unit keeps its worker's exit until it's stopped: a recorded verdict
/// (0 or 1) leaves it active, and anything else fails it, which collects
/// it.
pub struct Systemd {
    pub systemctl: std::path::PathBuf,
    pub unit: String,
}

impl Systemd {
    fn name(&self, id: &str) -> String {
        format!("{}@{id}.service", self.unit)
    }

    fn systemctl(&self, args: &[&str]) -> Result<String, String> {
        let out = Command::new(&self.systemctl)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("{}: {e}", self.systemctl.display()))?;
        if !out.status.success() {
            return Err(format!(
                "systemctl {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

impl Units for Systemd {
    fn run(&self, id: &str, work: &Work, until: Instant) -> Result<Ended, String> {
        // A oneshot's start returns once its worker has exited, whichever
        // way: how is read from systemd after.
        let mut start = Command::new(&self.systemctl);
        start.args(["start", &self.name(id)]);
        work.run(&mut start, None, until, 1 << 16)?;
        let shown = self.systemctl(&[
            "show",
            "-p",
            "ActiveState,SubState,Result,ExecMainCode,ExecMainStatus",
            &self.name(id),
        ])?;
        Ok(ended(&shown))
    }

    fn stop(&self, id: &str) -> Result<(), String> {
        self.systemctl(&["stop", &self.name(id)]).map(drop)
    }

    fn loaded(&self) -> Result<Vec<String>, String> {
        let prefix = format!("{}@", self.unit);
        let listed = self.systemctl(&[
            "list-units",
            "--all",
            "--plain",
            "--no-legend",
            &format!("{prefix}*.service"),
        ])?;
        Ok(listed
            .lines()
            .filter_map(|l| l.split_whitespace().next())
            .filter_map(|u| u.strip_prefix(&prefix)?.strip_suffix(".service"))
            .map(str::to_owned)
            .collect())
    }
}

/// How a unit ended, from `systemctl show`: its worker's exit only when it
/// exited on its own and systemd took that as the unit's success.
pub fn ended(shown: &str) -> Ended {
    let get = |k: &str| {
        shown
            .lines()
            .find_map(|l| l.strip_prefix(k)?.strip_prefix('='))
            .unwrap_or_default()
    };
    // ExecMainCode 1 is CLD_EXITED: an exit, not a signal.
    if get("ActiveState") == "active"
        && get("SubState") == "exited"
        && get("Result") == "success"
        && get("ExecMainCode") == "1"
        && let Ok(code) = get("ExecMainStatus").parse::<u8>()
    {
        return Ended::Exited(code);
    }
    Ended::Broke(format!(
        "its unit ended {} ({}), with no exit of its worker's kept",
        get("ActiveState"),
        get("Result")
    ))
}
