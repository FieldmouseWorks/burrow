//! What modules/backup.nix builds in.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Config {
    /// HOST-backup, the unit's name and the command's.
    pub name: String,
    pub host: String,
    /// Where it keeps its state: restic's cache, rclone's copy, what it did.
    pub state: PathBuf,
    pub repository: String,
    pub password_file: PathBuf,
    pub rclone_config_file: Option<PathBuf>,
    /// Which snapshots it backs up: a glob on their names.
    pub snapshots: String,
    pub datasets: Vec<String>,
    /// restic's exclude patterns, one a line.
    pub exclude_file: PathBuf,
    /// What the drill restores.
    pub drill: Vec<String>,
    /// restic's `--keep-NAME N`, by NAME.
    pub keep: BTreeMap<String, u32>,
    pub verify_days: u32,
    /// restic's pack size, in MiB.
    pub pack_size: u32,
    /// Hours the newest copy off the box may age before the owner hears.
    pub stale_after: u64,
    /// Where the drill restores to.
    pub scratch: PathBuf,
    /// proj, when the machine has it: a failure goes to a Claude job.
    pub proj: Option<PathBuf>,
    /// The machine's own project, the job's.
    pub project: String,
    pub owner: String,
    pub notify: PathBuf,
    /// Who runs the job.
    pub user: String,
    pub sudo: PathBuf,
    /// Where the programs it runs by name are (restic, zfs, util-linux's
    /// mount, unshare and runuser), and what they run (rclone, mount.zfs):
    /// ahead of the PATH it was given.
    pub path: String,
}

impl Config {
    pub fn read(path: &Path) -> Result<Config, String> {
        let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// restic's `--keep-NAME N` arguments.
    pub fn keep_args(&self) -> Vec<String> {
        self.keep
            .iter()
            .flat_map(|(k, n)| [format!("--keep-{k}"), n.to_string()])
            .collect()
    }
}

#[cfg(test)]
pub(crate) fn for_tests(state: &Path) -> Config {
    serde_json::from_value(serde_json::json!({
        "name": "box-backup",
        "host": "box",
        "state": state,
        "repository": "rclone:offsite:backups/box",
        "passwordFile": "/run/secrets/backup-password",
        "rcloneConfigFile": state.join("secret.conf"),
        "snapshots": "autosnap_*_hourly",
        "datasets": ["rpool/safe/data"],
        "excludeFile": "/nix/store/x-exclude",
        "drill": ["/data/dev/src/box"],
        "keep": {"hourly": 48, "daily": 30},
        "verifyDays": 30,
        "packSize": 64,
        "staleAfter": 6,
        "scratch": "/data/scratch",
        "proj": "/nix/store/x-proj/bin/proj",
        "project": "box",
        "owner": "Sam",
        "notify": "/nix/store/x-notify/bin/box-notify",
        "user": "dev",
        "sudo": "/run/wrappers/bin/sudo",
        "path": "/nix/store/x-restic/bin"
    }))
    .unwrap()
}

#[cfg(test)]
mod tests {
    #[test]
    fn keep_is_restics_flags_by_name() {
        let cfg = super::for_tests(std::path::Path::new("/s"));
        assert_eq!(
            cfg.keep_args(),
            ["--keep-daily", "30", "--keep-hourly", "48"]
        );
    }
}
