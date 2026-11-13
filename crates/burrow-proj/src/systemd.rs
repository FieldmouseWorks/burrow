//! The user's systemd manager, which runs each job's turns as units.

use std::env;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::config::Config;

/// A command to the user's manager. systemctl --user needs the user's
/// runtime dir, which `su` and cron don't set.
fn user(prog: &Path) -> Command {
    let mut c = Command::new(prog);
    c.arg("--user").stdin(Stdio::null());
    if env::var_os("XDG_RUNTIME_DIR").is_none() {
        c.env(
            "XDG_RUNTIME_DIR",
            format!("/run/user/{}", rustix::process::getuid().as_raw()),
        );
    }
    c
}

/// A systemctl --user command, its own output passed on.
pub fn systemctl(cfg: &Config) -> Command {
    user(&cfg.tools.systemctl)
}

/// A systemd-run --user command.
pub fn systemd_run(cfg: &Config) -> Command {
    user(&cfg.tools.systemd_run)
}

/// Whether the unit is up.
pub fn active(cfg: &Config, unit: &str) -> bool {
    systemctl(cfg)
        .args(["-q", "is-active", unit])
        .status()
        .is_ok_and(|s| s.success())
}

/// The units matching `pattern` that are up, by name as systemd spells
/// them, in one look: every state but inactive and failed, so a unit not
/// listed is one [`gone`] would say is. None when it can't be told.
pub fn up(cfg: &Config, pattern: &str) -> Option<std::collections::HashSet<String>> {
    let out = systemctl(cfg)
        .args([
            "list-units",
            "--plain",
            "--no-legend",
            "--no-pager",
            "--state=active,activating,deactivating,reloading,refreshing,maintenance",
            pattern,
        ])
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.split_whitespace().next().map(str::to_owned))
            .collect(),
    )
}

/// One of the unit's properties, "" when it can't be told.
pub fn show(cfg: &Config, unit: &str, property: &str) -> String {
    systemctl(cfg)
        .args(["show", "-P", property, unit])
        .stderr(Stdio::null())
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim_end().to_owned())
        .unwrap_or_default()
}

/// Whether a start or stop job is queued. Failure to observe is distinct
/// from a successfully observed empty Job property.
pub fn pending(cfg: &Config, unit: &str) -> Option<bool> {
    let output = systemctl(cfg)
        .args(["show", "-P", "Job", unit])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?;
    Some(!matches!(value.trim(), "" | "0"))
}

/// Known not to be running: not "can't say".
pub fn gone(cfg: &Config, unit: &str) -> bool {
    matches!(
        show(cfg, unit, "ActiveState").as_str(),
        "inactive" | "failed"
    )
}

/// A unit that failed is forgotten, so its name can be used again.
pub fn reset_failed(cfg: &Config, unit: &str) {
    let _ = systemctl(cfg)
        .args(["reset-failed", unit])
        .stderr(Stdio::null())
        .status();
}

/// Stop the unit; its exit status, 1 when systemctl couldn't be run.
pub fn stop(cfg: &Config, unit: &str) -> i32 {
    systemctl(cfg)
        .args(["stop", unit])
        .status()
        .map_or(1, |s| s.code().unwrap_or(1))
}
