//! `agent`: one disposable microVM per task, on its own ZFS clone of the data
//! dataset (burrow.agents.dataDataset, mounted at `data`).
//!
//! ```text
//! agent new PROJECT [--with DATA/PATH]... [--open]
//!                             a fresh sandbox for PROJECT; prints its id
//! agent claude ID [ARGS...]   Claude inside it, in the project, no prompts
//! agent sh ID [COMMAND...]    a shell inside it (or run one command); starts
//!                             it first if it was stopped
//! agent ls                    sandboxes: state, project, disk used, expiry
//! agent fetch ID              its branches and its uncommitted work, into the
//!                             project on the host under refs/agents/ID/
//! agent path ID               its copy of the project, as seen from the host
//! agent stop ID / start ID    power it off (the clone stays) / back on
//! agent rm ID                 stop it, save its work, delete it in a day
//! agent keep ID [DAYS]        undo an rm; keep it DAYS (default 7) from now
//! agent gc                    what the hourly timer runs
//! ```
//!
//! The VM sees the project (a clone, so nothing it does touches the real
//! one), its own home, and any --with paths under `data` (also from the
//! clone). Inside, Claude runs with permissions bypassed and the user has
//! sudo: the VM is the boundary. It reaches the network only through the
//! egress proxy, which allows the package registries, GitHub and Anthropic
//! (burrow.agents.allowlist); --open lifts the allowlist for that VM. It
//! holds no GitHub token: its work comes back with `agent fetch`, and you
//! push from the host.
//!
//! A sandbox nobody has used for 7 days is saved (agent fetch) and deleted by
//! `agent gc`; one left running unused for a day is stopped first.

pub mod list;
pub mod new;
pub mod path;
pub mod sandbox;
pub mod save;
pub mod ssh;

use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, SystemTime};

use serde::Deserialize;

/// A day, the unit of a sandbox's time to live.
pub const DAY: u64 = 86_400;

/// What modules/agents.nix builds in.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Config {
    /// The guest's microvm runner, linked into each sandbox as `current`.
    pub runner: PathBuf,
    /// What QEMU runs for each connection to the proxy address: one for the
    /// allowlist, one for `--open`.
    pub forward_filtered: PathBuf,
    pub forward_open: PathBuf,
    /// Where burrow.agents.dataset is mounted: a directory per sandbox.
    pub root: PathBuf,
    /// The projects folder, on the data dataset.
    pub src: PathBuf,
    /// Where the data dataset is mounted.
    pub data: PathBuf,
    pub data_dataset: String,
    pub dataset: String,
    /// The one user the sandboxes are for, its uid, group and home.
    pub user: String,
    pub uid: u32,
    pub group: String,
    pub home: PathBuf,
    /// Claude's login for the sandboxes, when the machine gives them one.
    pub token_file: Option<PathBuf>,
    pub sudo: PathBuf,
    /// Where the programs it runs by name are: zfs, git, ssh, ssh-keygen,
    /// socat, systemctl, runuser, install and chown. Ahead of the PATH it
    /// was given.
    pub path: String,
}

impl Config {
    pub fn read(path: &Path) -> Result<Config, String> {
        let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// The projects folder, from the data dataset's root: the same path in
    /// every clone.
    pub fn rel(&self) -> Result<&Path, String> {
        self.src.strip_prefix(&self.data).map_err(|_| {
            format!(
                "the projects folder {} isn't under {}",
                self.src.display(),
                self.data.display()
            )
        })
    }

    /// The key the user reaches the sandboxes with.
    pub fn key(&self) -> PathBuf {
        self.home.join(".ssh/agent_ed25519")
    }
}

/// One run of `agent`: its config, and how to start itself again.
pub struct Agent<'a> {
    pub cfg: &'a Config,
    exe: PathBuf,
    config: PathBuf,
    path: OsString,
}

impl<'a> Agent<'a> {
    pub fn new(cfg: &'a Config, config: &Path) -> Result<Agent<'a>, String> {
        let exe = std::env::current_exe().map_err(|e| format!("its own path: {e}"))?;
        let mut path = OsString::from(&cfg.path);
        if let Some(given) = std::env::var_os("PATH").filter(|p| !p.is_empty()) {
            path.push(":");
            path.push(given);
        }
        Ok(Agent {
            cfg,
            exe,
            config: config.to_owned(),
            path,
        })
    }

    /// `prog`, found on the config's path.
    pub fn command(&self, prog: impl AsRef<OsStr>) -> Command {
        let mut c = Command::new(prog);
        c.env("PATH", &self.path);
        c
    }

    /// This program again, with `args`.
    pub fn again<S: AsRef<OsStr>>(&self, args: &[S]) -> Command {
        let mut c = self.command(&self.exe);
        c.arg(&self.config).args(args);
        c
    }

    /// The same, through sudo: what only root may do.
    pub fn as_root_command<S: AsRef<OsStr>>(&self, args: &[S]) -> Command {
        let mut c = self.command(&self.cfg.sudo);
        c.arg("-n").arg(&self.exe).arg(&self.config).args(args);
        c
    }

    /// The same, as the user: runuser sets HOME from its entry.
    pub fn as_user_command<S: AsRef<OsStr>>(&self, args: &[S]) -> Command {
        let mut c = self.command("runuser");
        c.args(["-u", &self.cfg.user, "--"])
            .arg(&self.exe)
            .arg(&self.config)
            .args(args);
        c
    }

    /// Carries on as root: started as anyone else, it starts again through
    /// sudo.
    pub fn as_root<S: AsRef<OsStr>>(&self, args: &[S]) -> Result<(), String> {
        if rustix::process::geteuid().is_root() {
            return Ok(());
        }
        let e = self.as_root_command(args).exec();
        Err(format!("{}: {e}", self.cfg.sudo.display()))
    }

    /// Carries on as the user: started as anyone else (root), it starts
    /// again through runuser.
    pub fn as_user<S: AsRef<OsStr>>(&self, args: &[S]) -> Result<(), String> {
        if rustix::process::geteuid().as_raw() == self.cfg.uid {
            return Ok(());
        }
        let e = self.as_user_command(args).exec();
        Err(format!("runuser: {e}"))
    }

    /// Runs `prog` with `args`, its output to stderr (stdout is for what
    /// `agent` says, an id or a listing): an error when it fails.
    pub fn run<S: AsRef<OsStr>>(&self, prog: &str, args: &[S]) -> Result<(), String> {
        let status = self
            .command(prog)
            .args(args)
            .stdin(Stdio::null())
            .stdout(io::stderr())
            .status()
            .map_err(|e| format!("{prog}: {e}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("{} failed ({status})", shown(prog, args)))
        }
    }

    /// Whether `prog` with `args` succeeds, saying nothing.
    pub fn quietly<S: AsRef<OsStr>>(&self, prog: &str, args: &[S]) -> bool {
        self.command(prog)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }
}

fn shown<S: AsRef<OsStr>>(prog: &str, args: &[S]) -> String {
    let mut out = prog.to_owned();
    for a in args {
        out.push(' ');
        out.push_str(&a.as_ref().to_string_lossy());
    }
    out
}

/// An exit status as a shell gives it: a signal is 128 and its number.
pub fn exit_code(s: ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    s.code()
        .or_else(|| s.signal().map(|n| 128 + n))
        .unwrap_or(1)
}

/// A file's contents, its trailing newlines gone, as `$(cat FILE)` read it.
pub fn read(path: &Path) -> Result<String, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(text.trim_end_matches('\n').to_owned())
}

/// Writes `text` and a newline, as `echo TEXT >FILE` did.
pub fn write_line(path: &Path, text: &str) -> Result<(), String> {
    fs::write(path, format!("{text}\n")).map_err(|e| format!("{}: {e}", path.display()))
}

/// `touch FILE`: there, and modified now.
pub fn touch(path: &Path) -> Result<(), String> {
    let io = |e: io::Error| format!("{}: {e}", path.display());
    let f = File::options()
        .create(true)
        .append(true)
        .open(path)
        .map_err(io)?;
    f.set_modified(SystemTime::now()).map_err(io)
}

/// Seconds since the epoch.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
}

/// When a file was last modified, in seconds since the epoch, as
/// `stat -c %Y` said it.
pub fn mtime(path: &Path) -> Result<u64, String> {
    let t = fs::metadata(path)
        .and_then(|m| m.modified())
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(t.duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs())
}

/// The machine's name, as `uname -n` gives it.
pub fn hostname() -> String {
    rustix::system::uname()
        .nodename()
        .to_string_lossy()
        .into_owned()
}

/// Now, as `date -Is` writes it, in the machine's time zone.
pub fn stamp() -> String {
    jiff::Zoned::now()
        .strftime("%Y-%m-%dT%H:%M:%S%:z")
        .to_string()
}
