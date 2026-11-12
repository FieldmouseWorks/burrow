//! What the scan reads and runs: the folders, the sessions, the processes,
//! and systemd.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::decide::Git;
use crate::{Broke, Config};

/// The clock, in milliseconds since the epoch.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// An mtime as `find -printf %T@` writes it.
fn stamp(meta: &fs::Metadata) -> String {
    let at = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok());
    at.map_or("0.000000000".into(), |d| {
        format!("{}.{:09}", d.as_secs(), d.subsec_nanos())
    })
}

/// Everything a scan's look depends on, as lines: the sessions folder's
/// entries and when each last changed, the projects folder's entries, and when
/// each project's .git/config last changed. Through links, as the scan reads
/// them: the projects folder, a project's folder, its .git and its config may
/// each be one.
pub fn seen(sessions: &Path, projects: &Path) -> String {
    let mut out = String::new();
    let mut block = |mut lines: Vec<String>| {
        lines.sort();
        for l in lines {
            out.push_str(&l);
            out.push('\n');
        }
    };
    let entries = |dir: &Path| -> Vec<(PathBuf, OsString)> {
        fs::read_dir(dir)
            .map(|rd| rd.flatten().map(|e| (e.path(), e.file_name())).collect())
            .unwrap_or_default()
    };
    block(
        entries(sessions)
            .into_iter()
            .filter_map(|(p, _)| {
                let meta = fs::symlink_metadata(&p).ok()?;
                Some(format!("{} {}", p.display(), stamp(&meta)))
            })
            .collect(),
    );
    block(
        entries(projects)
            .into_iter()
            .map(|(p, _)| p.display().to_string())
            .collect(),
    );
    block(
        entries(projects)
            .into_iter()
            .filter_map(|(p, _)| {
                let config = p.join(".git/config");
                let meta = fs::metadata(&config).ok()?;
                Some(format!("{} {}", config.display(), stamp(&meta)))
            })
            .collect(),
    );
    out
}

/// realpath -m: links followed, the folders that aren't there kept as written.
pub fn realpath_m(path: &Path) -> PathBuf {
    let mut abs = PathBuf::new();
    if path.is_relative() {
        abs.push(std::env::current_dir().unwrap_or_default());
    }
    abs.push(path);
    let mut base = PathBuf::new();
    let mut rest = Vec::new();
    for part in abs.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                if rest.pop().is_none() {
                    base = base.parent().map(Path::to_path_buf).unwrap_or(base);
                }
            }
            Component::Normal(n) => {
                let next = base.join(n);
                if rest.is_empty() && next.exists() {
                    base = fs::canonicalize(&next).unwrap_or(next);
                } else {
                    rest.push(n.to_os_string());
                }
            }
            other => base.push(other.as_os_str()),
        }
    }
    base.extend(rest);
    base
}

/// The folders of the projects folder, as `PROJECTS/*/` finds them: not the
/// hidden ones, and links to folders count.
pub fn folders(projects: &Path) -> Vec<(OsString, PathBuf)> {
    let mut found: Vec<_> = fs::read_dir(projects)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| !e.file_name().as_bytes().starts_with(b".") && e.path().is_dir())
        .map(|e| (e.file_name(), e.path()))
        .collect();
    found.sort();
    found
}

pub fn git_of(folder: &Path) -> Git {
    let git = folder.join(".git");
    if git.is_dir() {
        Git::Dir
    } else if !git.exists() {
        Git::Absent
    } else {
        Git::Other
    }
}

/// A process's parent and state, from /proc/PID/stat.
fn stat(pid: u32) -> Option<(u32, char)> {
    let text = fs::read(format!("/proc/{pid}/stat")).ok()?;
    let text = String::from_utf8_lossy(&text);
    // "PID (COMM) STATE PPID ...", COMM being anything, parens too.
    let rest = text.get(text.rfind(')')? + 1..)?;
    let mut fields = rest.split_whitespace();
    let state = fields.next()?.chars().next()?;
    let ppid = fields.next()?.parse().ok()?;
    Some((ppid, state))
}

/// The processes whose parent is `pid`, with their state.
pub fn children(pid: u32) -> Vec<(u32, char)> {
    let mut kids: Vec<_> = fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter_map(|p| match stat(p)? {
            (ppid, state) if ppid == pid => Some((p, state)),
            _ => None,
        })
        .collect();
    kids.sort();
    kids
}

/// systemd and git, by the paths the config gives.
pub struct Tools<'a> {
    pub cfg: &'a Config,
}

impl Tools<'_> {
    fn output(&self, mut cmd: Command) -> Result<std::process::Output, Broke> {
        let name = cmd.get_program().to_owned();
        cmd.stdin(Stdio::null())
            .stderr(Stdio::inherit())
            .output()
            .map_err(|e| Broke(format!("run {}: {e}", Path::new(&name).display())))
    }

    fn systemctl(&self, args: &[&OsStr]) -> Command {
        let mut cmd = Command::new(&self.cfg.tools.systemctl);
        cmd.arg("--user").args(args);
        cmd
    }

    fn check(&self, cmd: Command, what: &str) -> Result<(), Broke> {
        let out = self.output(cmd)?;
        if out.status.success() {
            Ok(())
        } else {
            Err(Broke(format!("{what}: {}", out.status)))
        }
    }

    /// `systemctl --user VERB --no-block UNIT`.
    pub fn no_block(&self, verb: &str, unit: &OsStr) -> Result<(), Broke> {
        let cmd = self.systemctl(&[verb.as_ref(), "--no-block".as_ref(), unit]);
        self.check(cmd, &format!("systemctl {verb} {}", unit.to_string_lossy()))
    }

    /// The loaded units matching a glob, running or not.
    pub fn units(&self, glob: &str) -> Vec<OsString> {
        let cmd = self.systemctl(&[
            "list-units".as_ref(),
            "--plain".as_ref(),
            "--no-legend".as_ref(),
            "--all".as_ref(),
            glob.as_ref(),
        ]);
        let Ok(out) = self.output(cmd) else {
            return Vec::new();
        };
        out.stdout
            .split(|&b| b == b'\n')
            .filter_map(|line| line.split(|&b| b == b' ').find(|w| !w.is_empty()))
            .map(|w| OsString::from_vec(w.to_vec()))
            .collect()
    }

    /// A unit's main process, if it has one.
    pub fn main_pid(&self, unit: &OsStr) -> Option<u32> {
        let cmd = self.systemctl(&[
            "show".as_ref(),
            "-p".as_ref(),
            "MainPID".as_ref(),
            "--value".as_ref(),
            unit,
        ]);
        let out = self.output(cmd).ok().filter(|o| o.status.success())?;
        let pid: u32 = String::from_utf8_lossy(&out.stdout).trim().parse().ok()?;
        (pid != 0).then_some(pid)
    }

    /// Stop a timer there may not be.
    pub fn stop_quietly(&self, unit: &str) {
        let mut cmd = self.systemctl(&["stop".as_ref(), unit.as_ref()]);
        let _ = cmd.stdout(Stdio::null()).stderr(Stdio::null()).status();
    }

    /// The unit name systemd-escape gives a template's instance.
    pub fn escape(&self, template: &str, name: &OsStr) -> Result<OsString, Broke> {
        let mut cmd = Command::new(&self.cfg.tools.systemd_escape);
        cmd.args(["--template", template]).arg(name);
        let out = self.output(cmd)?;
        if !out.status.success() {
            return Err(Broke(format!(
                "systemd-escape {}: {}",
                name.to_string_lossy(),
                out.status
            )));
        }
        let mut unit = out.stdout;
        while unit.last() == Some(&b'\n') {
            unit.pop();
        }
        Ok(OsString::from_vec(unit))
    }

    /// Whether a checkout's config names an origin.
    pub fn has_origin(&self, config: &Path) -> bool {
        let mut cmd = Command::new(&self.cfg.tools.git);
        cmd.args(["config", "--file"])
            .arg(config)
            .args(["--get", "remote.origin.url"]);
        cmd.stdout(Stdio::null());
        self.output(cmd).is_ok_and(|o| o.status.success())
    }

    /// What `jq -r FILTER FILE` prints of a session's file, if it can read it.
    pub fn jq(&self, filter: &str, file: &Path) -> Option<String> {
        let mut cmd = Command::new(&self.cfg.tools.jq);
        cmd.args(["-r", filter]).arg(file).stderr(Stdio::null());
        let out = cmd
            .stdin(Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())?;
        Some(
            String::from_utf8_lossy(&out.stdout)
                .trim_end_matches('\n')
                .to_string(),
        )
    }

    /// Book the wake-up: a timer of its own for the scan, gone once it fires.
    /// In milliseconds: a timer never fires early, so the scan it starts
    /// finds the spell over.
    pub fn book(&self, left_ms: i64) -> Result<(), Broke> {
        let mut cmd = Command::new(&self.cfg.tools.systemd_run);
        cmd.args(["--user", "--quiet", "--unit=claude-rc-scan"])
            .arg(format!("--on-active={left_ms}ms"))
            .args([
                "--timer-property=AccuracySec=1s",
                "--timer-property=RemainAfterElapse=no",
            ]);
        self.check(cmd, "systemd-run")
    }
}
