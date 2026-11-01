//! What a pass reads and acts through: GitHub, systemd and burrow-events'
//! kick port.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::Value;

use crate::config::{Config, Fresh};

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
    /// The running system: the store path /run/current-system points to.
    fn running(&self) -> Result<String, String>;
    /// Whether a switch is in flight: one awaiting its confirm, or its revert
    /// at work. It restarts what it changes, runners among them.
    fn switching(&self) -> bool;
    /// `systemctl restart` (or `start`) the unit.
    fn restart(&self, unit: &str, verb: &str) -> Result<(), String>;
    /// Cancel the repo's Actions run.
    fn cancel(&self, repo: &str, run: u64) -> Result<(), String>;
    /// Start a burrow-events pass now (HANDLER[/OWNER/REPO]). Best effort.
    fn kick(&self, path: &str);
    /// The instances of a template's units systemd has loaded.
    fn instances(&self, template: &str) -> Vec<String>;
    /// Register a runner for one job as the instance's, and queue its
    /// unit's start with that registration, which the unit then owns.
    fn fresh(&self, f: &Fresh, instance: &str) -> Result<(), String>;
    /// Remove the JIT configs no loaded unit owns: what it says of each, by
    /// name.
    fn sweep(&self, f: &Fresh) -> Vec<String>;
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

    fn running(&self) -> Result<String, String> {
        let at = "/run/current-system";
        fs::canonicalize(at)
            .map(|p| p.to_string_lossy().into_owned())
            .map_err(|e| format!("{at}: {e}"))
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

    fn instances(&self, template: &str) -> Vec<String> {
        let out = Command::new("systemctl")
            .args(["list-units", "--all", "--plain", "--no-legend", "--full"])
            .arg(format!("{template}*.service"))
            .stderr(Stdio::inherit())
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        instances(&out, template)
    }

    fn fresh(&self, f: &Fresh, instance: &str) -> Result<(), String> {
        let name = format!("{}{instance}", f.prefix);
        let out = gh(&[
            "-X",
            "POST",
            &format!("orgs/{}/actions/runners/generate-jitconfig", f.owner),
            "-f",
            &format!("name={name}"),
            "-F",
            &format!("runner_group_id={}", f.group),
            "-f",
            &format!("labels[]={}", f.label),
        ])
        .map_err(|e| format!("registering {name}: {e}"))?;
        // The registration is the job's to take: it's never printed, and
        // waits only in a file this user alone reads until the unit has it.
        let config = serde_json::from_slice::<Value>(&out)
            .ok()
            .and_then(|v| v.get("encoded_jit_config")?.as_str().map(str::to_owned))
            .ok_or_else(|| format!("registering {name}: GitHub gave no JIT config"))?;
        let unit = format!("{}{instance}.service", f.template);
        hand(&f.configs.join(instance), &config, || {
            // Queued, not waited for: a box slow to set a unit's sandbox up
            // (remi#438: 90s each) would hold the pass past its timeout.
            let out = Command::new("systemctl")
                .args(["start", "--no-block", &unit])
                .output()
                .map_err(|e| format!("systemctl: {e}"))?;
            if out.status.success() {
                Ok(())
            } else {
                Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
            }
        })
    }

    fn sweep(&self, f: &Fresh) -> Vec<String> {
        sweep(&f.configs, &self.instances(&f.template))
    }
}

/// Removes each JIT config in `dir` but the `live` instances'. Once its
/// start is queued, a config is its unit's: the unit removes it as its
/// runner starts, or as it stops if it never got that far. One whose unit
/// systemd no longer has was left by a start that never ran (a pass cut
/// short between writing it and queueing the start, or a start job
/// cancelled), and its registration is still one a process could take a
/// job with. What it says of each names it, never what it held.
fn sweep(dir: &Path, live: &[String]) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut said: Vec<String> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if live.contains(&name) {
                return None;
            }
            let why = "a JIT config no unit has";
            match fs::remove_file(e.path()) {
                Ok(()) => Some(format!("removed {name}, {why}")),
                // Its unit stopped, and took it, after systemd was asked.
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
                Err(err) => Some(format!("couldn't remove {name}, {why}: {err}")),
            }
        })
        .collect();
    said.sort();
    said
}

/// Leaves `config` at `path`, readable by this user alone, for the unit
/// `start` queues to load and then remove. A start that fails removes it
/// here, since no unit will. A file already there is someone else's, and
/// stays.
fn hand(
    path: &Path,
    config: &str,
    start: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .and_then(|mut file| file.write_all(config.as_bytes()))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    start().inspect_err(|_| {
        if let Err(e) = fs::remove_file(path) {
            println!("couldn't remove {}: {e}", path.display());
        }
    })
}

/// The instances of a template (`NAME@`) in `systemctl list-units --plain
/// --no-legend` lines: each line's first word is a unit.
fn instances(out: &str, template: &str) -> Vec<String> {
    out.lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter_map(|u| u.strip_prefix(template)?.strip_suffix(".service"))
        .filter(|i| !i.is_empty())
        .map(str::to_owned)
        .collect()
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
    use super::{hand, instances, shown, sweep, working};

    // A JIT config is there for its unit's start, readable by this user
    // alone, and stays for the unit once the start is queued; a start that
    // fails takes it away. One already there isn't touched.
    #[test]
    fn hands_a_config_over() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("17-0");
        for result in [Err("Access denied".to_owned()), Ok(())] {
            let got = hand(&path, "secret", || {
                let meta = std::fs::metadata(&path).unwrap();
                assert_eq!(meta.permissions().mode() & 0o777, 0o600);
                assert_eq!(std::fs::read_to_string(&path).unwrap(), "secret");
                result.clone()
            });
            assert_eq!(got, result);
            assert_eq!(path.exists(), result.is_ok());
        }
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, "theirs").unwrap();
        let got = hand(&path, "secret", || panic!("started"));
        assert!(got.unwrap_err().contains("exists"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "theirs");
    }

    // Every config no loaded unit has, readable or not, is removed, and
    // named; a live unit's stays. A directory with none, or none at all,
    // says nothing.
    #[test]
    fn sweeps_what_no_unit_has() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        assert!(sweep(&dir.path().join("gone"), &[]).is_empty());
        assert!(sweep(dir.path(), &[]).is_empty());
        for (name, mode) in [("17-1", 0o600), ("17-0", 0o400), ("18-0", 0o600)] {
            let path = dir.path().join(name);
            std::fs::write(&path, "secret").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        let said = sweep(dir.path(), &["18-0".into(), "19-0".into()]);
        assert_eq!(
            said,
            [
                "removed 17-0, a JIT config no unit has",
                "removed 17-1, a JIT config no unit has"
            ]
        );
        assert!(said.iter().all(|s| !s.contains("secret")));
        let left: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left, ["18-0"]);
    }

    // A template's instances, and nothing else's: another template's, the
    // template itself, or a unit that only starts like it.
    #[test]
    fn reads_instances() {
        let out = "\
fresh-runner@17-0.service loaded active running GitHub Actions runner
fresh-runner@17-1.service loaded failed failed GitHub Actions runner
fresh-runner@.service loaded inactive dead
fresh-runner-old@3.service loaded active running other
";
        assert_eq!(instances(out, "fresh-runner@"), ["17-0", "17-1"]);
        assert!(instances("", "fresh-runner@").is_empty());
    }

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
