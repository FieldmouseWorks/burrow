//! firmware-check: tells the owner when there's firmware to flash. Flashing
//! stays their call (a bad flash can brick a board with no console), so it
//! only looks.
//!
//! - check.rs: daily, what fwupd (LVFS) offers for the machine, as a notice
//! - review.rs: monthly, a Claude job looks at what LVFS doesn't carry
//!   (board BIOS, NVMe, Intel ME or AMD PSP)
//!
//! It runs as root; what it runs, it runs by path (`Config::tools`).

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

pub mod check;
pub mod review;

/// What modules/updates.nix tells firmware-check about the machine.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Config {
    /// firmware-check, as the machine names it: how it says itself.
    pub name: String,
    /// The machine's host name, for its notices and the review's task.
    pub host: String,
    /// What fwupd offered when the owner was last told, so a day that
    /// offers the same tells them nothing new.
    pub offered: PathBuf,
    /// The machine's user, whose Claude job the review is.
    pub user: String,
    /// The project the review's job is in.
    pub project: String,
    /// The App the review's job acts on GitHub as (proj run --app), when
    /// the machine has one.
    pub app: Option<String>,
    /// Who decides on flashing, as the review's task names them.
    pub owner: String,
    /// HOST-notify as the review's job runs it, by name.
    pub notify: String,
    /// Where the review should look for firmware LVFS doesn't carry.
    pub review_hints: String,
    /// The kernel's /proc/cpuinfo, for the CPU's model.
    pub cpuinfo: PathBuf,
    pub tools: Tools,
}

/// What it runs, by path.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Tools {
    /// What firmware-check started by anyone but root starts again through.
    pub sudo: PathBuf,
    /// HOST-notify, which tells the owner.
    pub notify: PathBuf,
    pub fwupdmgr: PathBuf,
    pub dmidecode: PathBuf,
    pub nvme: PathBuf,
    /// What runs proj as the user.
    pub runuser: PathBuf,
    pub proj: PathBuf,
}

impl Config {
    pub fn read(path: &Path) -> Result<Config, String> {
        let text = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_slice(&text).map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// Something it needed broke: it stops there, says why and exits 1.
#[derive(Debug, PartialEq, Eq)]
pub struct Broke(pub String);

/// A JSON value as jq's string interpolation shows it: a string as itself,
/// anything else (none at all as null) as JSON.
pub(crate) fn shown(v: Option<&serde_json::Value>) -> String {
    match v {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
        None => "null".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn its_config_is_every_setting_and_only_those() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("firmware.json");
        let config = serde_json::json!({
            "name": "firmware-check", "host": "box", "offered": "/var/lib/b/firmware-offered",
            "user": "sam", "project": "box", "app": null, "owner": "Sam",
            "notify": "box-notify", "reviewHints": "the board's page", "cpuinfo": "/proc/cpuinfo",
            "tools": {"sudo": "/s", "notify": "/n", "fwupdmgr": "/f", "dmidecode": "/d",
                      "nvme": "/v", "runuser": "/r", "proj": "/p"},
        });
        fs::write(&path, config.to_string()).unwrap();
        let cfg = Config::read(&path).unwrap();
        assert_eq!(cfg.app, None);
        assert_eq!(cfg.tools.fwupdmgr, PathBuf::from("/f"));
        let mut more = config.clone();
        more["stray"] = true.into();
        fs::write(&path, more.to_string()).unwrap();
        assert!(
            Config::read(&path)
                .unwrap_err()
                .contains("unknown field `stray`")
        );
        let mut less = config;
        less["tools"].as_object_mut().unwrap().remove("nvme");
        fs::write(&path, less.to_string()).unwrap();
        assert!(
            Config::read(&path)
                .unwrap_err()
                .contains("missing field `nvme`")
        );
        assert!(Config::read(&dir.path().join("none.json")).is_err());
    }

    #[test]
    fn a_value_is_shown_as_jq_interpolates_it() {
        use serde_json::json;
        assert_eq!(shown(Some(&json!("a \"b\""))), "a \"b\"");
        assert_eq!(shown(Some(&json!(null))), "null");
        assert_eq!(shown(None), "null");
        assert_eq!(shown(Some(&json!(12))), "12");
        assert_eq!(shown(Some(&json!(false))), "false");
        assert_eq!(shown(Some(&json!({"a": [1, "x"]}))), r#"{"a":[1,"x"]}"#);
    }
}
