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
    /// One running whose processes can't be read counts as at one.
    pub busy: bool,
}

pub trait World {
    /// GET this path of GitHub's REST API (`repos/OWNER/REPO/...`): its JSON.
    fn github(&self, path: &str) -> Result<Value, String>;
    fn unit(&self, unit: &str) -> Unit;
    /// The running system's configuration revision, when it's a commit:
    /// none when it's from a dirty tree or unknown, an error when it can't
    /// be read.
    fn revision(&self) -> Result<Option<String>, String>;
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
        // A unit that isn't running has no processes; one that is, but
        // whose processes can't be read, may be at a job.
        let running = matches!(
            state.as_str(),
            "active" | "activating" | "deactivating" | "reloading" | "refreshing"
        );
        let busy = running
            && shown
                .get("ControlGroup")
                .filter(|cg| !cg.is_empty())
                .is_none_or(|cg| {
                    working(&Path::new("/sys/fs/cgroup").join(cg.trim_start_matches('/')))
                        .unwrap_or(true)
                });
        Unit { state, since, busy }
    }

    fn revision(&self) -> Result<Option<String>, String> {
        // As HOST-update reads it; it says "unknown" with a failure when
        // the system has none.
        let out = Command::new("/run/current-system/sw/bin/nixos-version")
            .arg("--configuration-revision")
            .stderr(Stdio::null())
            .output()
            .map_err(|e| format!("nixos-version: {e}"))?;
        let rev = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        Ok(
            (out.status.success() && rev.len() == 40 && rev.bytes().all(|b| b.is_ascii_hexdigit()))
                .then_some(rev),
        )
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

/// Whether a Runner.Worker process is anywhere in this cgroup: an error
/// when the cgroup, or the name of a process in it, can't be read. A
/// process whose name is missing is one gone, unless the cgroup still has
/// it: then /proc hides it from us.
fn working(cg: &Path) -> std::io::Result<bool> {
    let procs = |dir: &Path| -> std::io::Result<Vec<String>> {
        Ok(fs::read_to_string(dir.join("cgroup.procs"))?
            .lines()
            .map(|p| p.trim().to_owned())
            .collect())
    };
    let mut missing = Vec::new();
    for pid in procs(cg)? {
        match fs::read_to_string(format!("/proc/{pid}/comm")) {
            Ok(comm) if comm.trim() == "Runner.Worker" => return Ok(true),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => missing.push(pid),
            Err(e) => return Err(e),
        }
    }
    if let Some(pid) = procs(cg)?.into_iter().find(|p| missing.contains(p)) {
        return Err(std::io::Error::other(format!(
            "process {pid} is in {} but not in /proc",
            cg.display()
        )));
    }
    for e in fs::read_dir(cg)? {
        let e = e?;
        if e.file_type()?.is_dir() && working(&e.path())? {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::{shown, working};

    // A cgroup whose processes can't be read, or that has one /proc hides,
    // isn't taken for one at no job; one whose processes are all something
    // else is.
    #[test]
    fn reads_cgroups() {
        let dir = tempfile::tempdir().unwrap();
        assert!(working(&dir.path().join("gone")).is_err());
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(
            dir.path().join("cgroup.procs"),
            format!("{}\n", std::process::id()),
        )
        .unwrap();
        assert!(working(dir.path()).is_err(), "sub has no cgroup.procs");
        std::fs::write(sub.join("cgroup.procs"), "").unwrap();
        assert!(!working(dir.path()).unwrap());
        // A process the cgroup has and /proc doesn't show.
        std::fs::write(sub.join("cgroup.procs"), "4294967295\n").unwrap();
        assert!(working(dir.path()).is_err());
    }

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
