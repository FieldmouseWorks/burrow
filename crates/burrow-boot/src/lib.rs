//! HOST-boot: GRUB only ever defaults to a kernel that has already booted on
//! the machine. A new kernel, initrd or kernel command line gets tried once,
//! at a quiet hour, and becomes the default only after it comes up healthy.
//! If it panics or hangs, the panic timer or the hardware watchdog resets the
//! board and GRUB boots the last good one.
//!
//! - boot.rs: its words, `default`, `trial`, `check` and `status`, on what
//!   it keeps in its state dir
//! - menu.rs: which GRUB menu entry boots a system
//!
//! It runs as root; what it runs, it runs by path (`Config::tools`).

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

pub mod boot;
pub mod menu;

/// What modules/switch.nix tells HOST-boot about the machine.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Config {
    /// HOST-boot, as the machine names it: how it says itself, and its tag
    /// in the journal.
    pub boot: String,
    /// The machine's host name, for its notices.
    pub host: String,
    /// What it keeps between boots: the kernels that booted here, the ones
    /// that failed, the default it made and the trial it queued.
    pub state: PathBuf,
    /// Where HOST-switch keeps its state: a switch is pending while it has
    /// a `pending` file, and a trial boot waits for it.
    pub switch_state: PathBuf,
    /// The machine's user, whose Claude jobs (proj-*.service) a trial boot
    /// waits for.
    pub user: String,
    /// The system that booted (/run/booted-system) and the one that runs
    /// (/run/current-system).
    pub booted: PathBuf,
    pub current: PathBuf,
    /// GRUB's generated menu, and the dirs holding a grub dir whose grubenv
    /// says what boots next (/boot, and a mirrored /boot-fallback).
    pub grub_cfg: PathBuf,
    pub grub_dirs: Vec<PathBuf>,
    pub tools: Tools,
}

/// What it runs, by path.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Tools {
    /// What HOST-boot started by anyone but root starts again through.
    pub sudo: PathBuf,
    pub grub_editenv: PathBuf,
    pub systemctl: PathBuf,
    /// HOST-notify, which tells the owner.
    pub notify: PathBuf,
    /// systemd-cat, for what it says in the journal.
    pub logger: PathBuf,
}

impl Config {
    pub fn read(path: &Path) -> Result<Config, String> {
        let text = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_slice(&text).map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// Something a word needed broke: it stops there, says why and exits 1.
#[derive(Debug, PartialEq, Eq)]
pub struct Broke(pub String);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn its_config_is_every_setting_and_only_those() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("boot.json");
        let config = serde_json::json!({
            "boot": "box-boot", "host": "box", "state": "/var/lib/burrow",
            "switchState": "/run/box-switch", "user": "sam",
            "booted": "/run/booted-system", "current": "/run/current-system",
            "grubCfg": "/boot/grub/grub.cfg", "grubDirs": ["/boot", "/boot-fallback"],
            "tools": {"sudo": "/s", "grubEditenv": "/g", "systemctl": "/c", "notify": "/n", "logger": "/l"},
        });
        fs::write(&path, config.to_string()).unwrap();
        let cfg = Config::read(&path).unwrap();
        assert_eq!(
            cfg.grub_dirs,
            [PathBuf::from("/boot"), "/boot-fallback".into()]
        );
        assert_eq!(cfg.tools.grub_editenv, PathBuf::from("/g"));
        let mut more = config.clone();
        more["stray"] = true.into();
        fs::write(&path, more.to_string()).unwrap();
        assert!(
            Config::read(&path)
                .unwrap_err()
                .contains("unknown field `stray`")
        );
        let mut less = config;
        less["tools"].as_object_mut().unwrap().remove("notify");
        fs::write(&path, less.to_string()).unwrap();
        assert!(
            Config::read(&path)
                .unwrap_err()
                .contains("missing field `notify`")
        );
        assert!(Config::read(&dir.path().join("none.json")).is_err());
    }
}
