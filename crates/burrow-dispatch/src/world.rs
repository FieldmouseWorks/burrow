//! What a pass reads and acts through: GitHub, systemd and burrow-events'
//! kick port.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::Value;

use crate::config::Config;

/// A unit as systemd has it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Unit {
    /// Its ActiveState: active, activating (a oneshot at work), failed...
    pub state: String,
    /// When it became active, or, while it's activating, when it started.
    pub since: Option<f64>,
    /// Whether a process named Runner.Worker is in it: a runner at a job.
    pub busy: bool,
}

pub trait World {
    /// GET this path of GitHub's REST API (`repos/OWNER/REPO/...`): its JSON.
    fn github(&self, path: &str) -> Result<Value, String>;
    fn unit(&self, unit: &str) -> Unit;
    /// Whether a switch is in flight: one awaiting its confirm, or its revert
    /// at work. It restarts what it changes, runners among them.
    fn switching(&self) -> bool;
    /// `systemctl restart` (or `start`) the unit.
    fn restart(&self, unit: &str, verb: &str) -> Result<(), String>;
    /// Cancel the repo's Actions run.
    fn cancel(&self, repo: &str, run: u64) -> Result<(), String>;
    /// Start a burrow-events pass now (HANDLER[/OWNER/REPO]). Best effort.
    fn kick(&self, path: &str);
}

/// The machine's own: gh (the machinery's, on the PATH), systemctl, and the
/// cgroup tree.
pub struct Real<'a>(pub &'a Config);

/// `gh api` with these arguments: what it printed, or why it failed.
fn gh(args: &[&str]) -> Result<Vec<u8>, String> {
    let out = Command::new("gh")
        .args(["api", "-H", "Accept: application/vnd.github+json"])
        .args(args)
        .output()
        .map_err(|e| format!("gh: {e}"))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
    }
}

impl World for Real<'_> {
    fn github(&self, path: &str) -> Result<Value, String> {
        let out = gh(&[path]).map_err(|e| format!("GET {path}: {e}"))?;
        serde_json::from_slice(&out).map_err(|e| format!("GET {path}: {e}"))
    }

    fn unit(&self, unit: &str) -> Unit {
        let props = [
            "ActiveState",
            "ActiveEnterTimestamp",
            "InactiveExitTimestamp",
            "ControlGroup",
        ];
        let out = Command::new("systemctl")
            .arg("show")
            .args(props.map(|p| format!("--property={p}")))
            .args(["--timestamp=unix", unit])
            .stderr(Stdio::inherit())
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        let shown = shown(&out);
        let at = |k: &str| {
            shown
                .get(k)
                .and_then(|v| v.strip_prefix('@'))
                .and_then(|v| v.parse().ok())
        };
        let state = shown.get("ActiveState").cloned().unwrap_or_default();
        let since = if state == "activating" {
            at("InactiveExitTimestamp")
        } else {
            at("ActiveEnterTimestamp")
        };
        let busy = shown
            .get("ControlGroup")
            .filter(|cg| !cg.is_empty())
            .is_some_and(|cg| {
                working(&Path::new("/sys/fs/cgroup").join(cg.trim_start_matches('/')))
            });
        Unit { state, since, busy }
    }

    fn switching(&self) -> bool {
        let Some(switch) = &self.0.switch else {
            return false;
        };
        // One it can't tell about counts as in flight: a restart then would
        // be in its way.
        switch.state.join("pending").try_exists().unwrap_or(true)
            || matches!(
                self.unit(&switch.revert).state.as_str(),
                "activating" | "active" | "deactivating"
            )
    }

    fn restart(&self, unit: &str, verb: &str) -> Result<(), String> {
        let out = Command::new("systemctl")
            .args([verb, unit])
            .output()
            .map_err(|e| format!("systemctl: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
        }
    }

    fn cancel(&self, repo: &str, run: u64) -> Result<(), String> {
        gh(&[
            "-X",
            "POST",
            &format!("repos/{repo}/actions/runs/{run}/cancel"),
        ])
        .map(drop)
    }

    fn kick(&self, path: &str) {
        let url = format!("{}/{path}", self.0.kick);
        if let Err(e) = burrow_events::kick::kick(&url, Duration::from_secs(5)) {
            println!("couldn't kick {path}: {e}");
        }
    }
}

/// `systemctl show`'s KEY=VALUE lines.
fn shown(out: &str) -> BTreeMap<String, String> {
    out.lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}

/// Whether a Runner.Worker process is anywhere in this cgroup.
fn working(cg: &Path) -> bool {
    let here = fs::read_to_string(cg.join("cgroup.procs"))
        .unwrap_or_default()
        .lines()
        .any(|pid| {
            fs::read_to_string(format!("/proc/{}/comm", pid.trim()))
                .is_ok_and(|comm| comm.trim() == "Runner.Worker")
        });
    here || fs::read_dir(cg).is_ok_and(|d| {
        d.flatten()
            .any(|e| e.file_type().is_ok_and(|t| t.is_dir()) && working(&e.path()))
    })
}

#[cfg(test)]
mod tests {
    use super::shown;

    #[test]
    fn reads_systemctl_show() {
        let got = shown(
            "ActiveState=active\nActiveEnterTimestamp=@1800000000\nControlGroup=/system.slice/r.service\n",
        );
        assert_eq!(got["ActiveState"], "active");
        assert_eq!(got["ActiveEnterTimestamp"], "@1800000000");
        assert_eq!(got["ControlGroup"], "/system.slice/r.service");
    }
}
