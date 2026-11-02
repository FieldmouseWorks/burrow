//! A dead man's switch for system changes. HOST-switch puts a new system
//! live without making it the boot default, and unless HOST-confirm runs in
//! time the old one comes back by itself. So an agent can change anything
//! about the box, including what it needs to reach it, and a mistake undoes
//! itself.
//!
//! - switch.rs: HOST-switch's words: a switch, `--status`, `--abort`, and
//!   what its own units run (`--revert-to`, `--activation-stopped`)
//! - confirm.rs: HOST-confirm
//!
//! What they keep is in `Config::state`, while a switch is unconfirmed:
//! `pending` (the system put live), `prev` (the confirmed one it reverts
//! to), `revert-failed` (the confirmed one, when a revert didn't get back to
//! it) and `lock`, which a switch and a confirm hold while they work. Both
//! run as root; what they run, they run by path (`Config::tools`).

use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::{env, thread, time::Duration};

use rustix::fs::{FlockOperation, flock};
use serde::Deserialize;

pub mod confirm;
pub mod switch;

/// What modules/switch.nix tells HOST-switch and HOST-confirm about the
/// machine. Both binaries take the same.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Config {
    /// HOST-switch and HOST-confirm, as the machine names them: how they
    /// say themselves, and the switch's tag in the journal.
    pub switch: String,
    pub confirm: String,
    /// The revert's units: REVERT.timer, which a switch arms, and
    /// REVERT.service, which it starts (and REVERT-now, an abort's).
    pub revert: String,
    /// Where they keep a switch while it's unconfirmed.
    pub state: PathBuf,
    /// The machine's flake, and which of its nixosConfigurations a switch
    /// builds.
    pub flake: PathBuf,
    pub configuration: String,
    /// How long a switch waits for HOST-confirm before it reverts (a
    /// systemd time span), unless BURROW_CONFIRM_WITHIN says otherwise.
    pub within: String,
    /// The system that runs (/run/current-system), and the system profile,
    /// whose generation is the last confirmed system.
    pub current: PathBuf,
    pub profile: PathBuf,
    /// The lock switch-to-configuration holds while it activates.
    pub activation_lock: PathBuf,
    /// The PATH a revert's units run with: systemd starts them with a bare
    /// one.
    pub path: String,
    pub tools: Tools,
}

/// What they run, by path.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Tools {
    /// What they start again through when started by anyone but root.
    pub sudo: PathBuf,
    pub systemctl: PathBuf,
    pub systemd_run: PathBuf,
    pub systemd_analyze: PathBuf,
    /// systemd-cat, for what the switch says in the journal.
    pub logger: PathBuf,
    /// What a switch builds the flake as its checkout's owner with.
    pub runuser: PathBuf,
    pub nix: PathBuf,
    pub nix_env: PathBuf,
    /// HOST-boot, with trial boots: what makes a confirmed system the boot
    /// default (a new kernel boots once on trial first). Without, the
    /// system's own switch-to-configuration does.
    pub boot: Option<PathBuf>,
}

impl Config {
    pub fn read(path: &Path) -> Result<Config, String> {
        let text = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_slice(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    fn file(&self, name: &str) -> PathBuf {
        self.state.join(name)
    }

    /// The system in the state file `name`, if there's one.
    fn recorded(&self, name: &str) -> Result<Option<PathBuf>, Broke> {
        let path = self.file(name);
        match fs::read_to_string(&path) {
            Ok(text) => {
                let text = text.trim_end_matches('\n');
                Ok((!text.is_empty()).then(|| text.into()))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(at(&path, e)),
        }
    }

    fn write(&self, name: &str, system: &Path) -> Result<(), Broke> {
        let path = self.file(name);
        fs::write(&path, format!("{}\n", system.display())).map_err(|e| at(&path, e))
    }

    fn remove(&self, name: &str) -> Result<(), Broke> {
        let path = self.file(name);
        match fs::remove_file(&path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(at(&path, e)),
            _ => Ok(()),
        }
    }

    /// The system that runs now.
    fn running(&self) -> Result<PathBuf, Broke> {
        canonical(&self.current)
    }

    /// The state lock, held until it's dropped: one switch or confirm at a
    /// time, each owning `pending` and the revert's timer until it's done.
    fn lock(&self) -> Result<File, Broke> {
        fs::create_dir_all(&self.state).map_err(|e| at(&self.state, e))?;
        let path = self.file("lock");
        let lock = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .map_err(|e| at(&path, e))?;
        flock(&lock, FlockOperation::LockExclusive)
            .map_err(|e| Broke(format!("{}: {e}", path.display())))?;
        Ok(lock)
    }

    /// systemctl's `property` of `unit`: nothing when it doesn't say.
    fn show(&self, unit: &str, property: &str) -> String {
        Command::new(&self.tools.systemctl)
            .args(["show", "-p", property, "--value", unit])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    }

    /// systemctl `words`, whether it worked or not (a unit that isn't
    /// there, say).
    fn systemctl_quiet(&self, words: &[&str]) {
        let _ = Command::new(&self.tools.systemctl)
            .args(words)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    /// Whether the revert is running (a oneshot: activating for as long as
    /// it runs).
    fn reverting(&self) -> bool {
        matches!(
            self.show(&self.service(), "ActiveState").as_str(),
            "activating" | "active" | "deactivating"
        )
    }

    /// Until the revert isn't `running`, looking every second.
    fn wait_while(&self, running: impl Fn(&Config) -> bool) {
        while running(self) {
            thread::sleep(Duration::from_secs(1));
        }
    }

    fn timer(&self) -> String {
        format!("{}.timer", self.revert)
    }

    fn service(&self) -> String {
        format!("{}.service", self.revert)
    }

    /// `line`, in the journal at `priority`, tagged as HOST-switch; on
    /// stderr when the journal won't take it.
    fn journal(&self, priority: &str, line: &str) {
        let logged = Command::new(&self.tools.logger)
            .args(["-t", &self.switch, "-p", priority])
            .stdin(Stdio::piped())
            .spawn()
            .and_then(|mut c| {
                if let Some(mut stdin) = c.stdin.take() {
                    writeln!(stdin, "{line}")?;
                }
                c.wait()
            });
        if !logged.is_ok_and(|s| s.success()) {
            complain(line);
        }
    }

    /// `system` made the boot default: through HOST-boot with trial boots,
    /// else its own switch-to-configuration.
    fn boot_default(&self, system: &Path) -> Result<(), Broke> {
        match &self.tools.boot {
            Some(boot) => ran(Command::new(boot).arg("default").arg(system)),
            None => ran(Command::new(system.join("bin/switch-to-configuration")).arg("boot")),
        }
    }

    /// The system profile pointed at `system`, through the daemon as the
    /// user's builds are: root opening the store directly can trip over how
    /// /nix/store is mounted.
    fn set_profile(&self, system: &Path) -> Result<(), Broke> {
        ran(Command::new(&self.tools.nix_env)
            .env("NIX_REMOTE", "daemon")
            .arg("-p")
            .arg(&self.profile)
            .arg("--set")
            .arg(system))
    }
}

/// Something it needed broke: it stops there, says why and exits 1.
#[derive(Debug, PartialEq, Eq)]
pub struct Broke(pub String);

/// The word one started by anyone but root starts again with, through sudo.
/// sudo makes it root. Where it doesn't (a runas default, or the tests'
/// stand-in), it runs as whoever it made it, rather than starting again for
/// ever.
pub const THROUGH_SUDO: &str = "__through-sudo";

/// Started again as root through sudo, as `CONFIG __through-sudo WORDS`,
/// unless it's root already or came through sudo: what it says when it
/// couldn't, or nothing to go on as it is.
pub fn as_root(
    sudo: &Path,
    config: &str,
    through_sudo: bool,
    words: &[impl AsRef<OsStr>],
) -> Option<String> {
    if through_sudo || rustix::process::geteuid().is_root() {
        return None;
    }
    let exe = match env::current_exe() {
        Ok(exe) => exe,
        Err(e) => return Some(format!("its own path: {e}")),
    };
    let e = Command::new(sudo)
        .arg("--")
        .arg(exe)
        .arg(config)
        .arg(THROUGH_SUDO)
        .args(words)
        .exec();
    Some(format!("{}: {e}", sudo.display()))
}

/// `path` with every symlink in it followed.
fn canonical(path: &Path) -> Result<PathBuf, Broke> {
    fs::canonicalize(path).map_err(|e| at(path, e))
}

/// `c` run, its output its own: whether it worked.
fn ran(c: &mut Command) -> Result<(), Broke> {
    let shown = shown(c);
    match c.stdin(Stdio::null()).status() {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(Broke(format!("{shown}: {s}"))),
        Err(e) => Err(Broke(format!("{shown}: {e}"))),
    }
}

/// `c`'s program and words, as said when it fails.
fn shown(c: &Command) -> String {
    let mut s = c.get_program().to_string_lossy().into_owned();
    for a in c.get_args() {
        s.push(' ');
        s.push_str(&a.to_string_lossy());
    }
    s
}

/// What a program exited with, as a shell's `$?` says it: 128 and the
/// signal for one killed, 127 for one that didn't start.
fn exit_of(status: io::Result<std::process::ExitStatus>) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    match status {
        Ok(s) => s.code().or(s.signal().map(|n| 128 + n)).unwrap_or(1),
        Err(_) => 127,
    }
}

fn at(path: &Path, e: io::Error) -> Broke {
    Broke(format!("{}: {e}", path.display()))
}

/// What it says: on stdout, whether or not anything reads it.
fn say(line: &str) {
    let _ = writeln!(io::stdout().lock(), "{line}");
}

fn complain(line: &str) {
    let _ = writeln!(io::stderr().lock(), "{line}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn its_config_is_every_setting_and_only_those() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("switch.json");
        let config = serde_json::json!({
            "switch": "box-switch", "confirm": "box-confirm", "revert": "box-revert",
            "state": "/run/box-switch", "flake": "/etc/nixos", "configuration": "box",
            "within": "10min", "current": "/run/current-system",
            "profile": "/nix/var/nix/profiles/system",
            "activationLock": "/run/nixos/switch-to-configuration.lock", "path": "/b",
            "tools": {"sudo": "/s", "systemctl": "/c", "systemdRun": "/r", "systemdAnalyze": "/a",
                      "logger": "/l", "runuser": "/u", "nix": "/n", "nixEnv": "/e", "boot": null},
        });
        fs::write(&path, config.to_string()).unwrap();
        let cfg = Config::read(&path).unwrap();
        assert_eq!(cfg.tools.boot, None);
        assert_eq!(cfg.tools.systemd_run, PathBuf::from("/r"));
        let mut more = config.clone();
        more["stray"] = true.into();
        fs::write(&path, more.to_string()).unwrap();
        assert!(
            Config::read(&path)
                .unwrap_err()
                .contains("unknown field `stray`")
        );
        let mut less = config;
        less["tools"].as_object_mut().unwrap().remove("nixEnv");
        fs::write(&path, less.to_string()).unwrap();
        assert!(
            Config::read(&path)
                .unwrap_err()
                .contains("missing field `nixEnv`")
        );
        assert!(Config::read(&dir.path().join("none.json")).is_err());
    }

    #[test]
    fn an_exit_is_what_a_shell_would_say_it_was() {
        let sh = |script: &str| Command::new("sh").args(["-c", script]).status();
        assert_eq!(exit_of(sh("exit 11")), 11);
        assert_eq!(exit_of(sh("kill -9 $$")), 137);
        assert_eq!(exit_of(Command::new("/nowhere/at-all").status()), 127);
    }
}
