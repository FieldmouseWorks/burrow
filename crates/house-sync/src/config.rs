//! What modules/house.nix tells a pass, as JSON.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::exclude::Exclude;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Config {
    /// The blocks this pass keeps: burrow's house/, as this build has it.
    pub house: PathBuf,
    /// The blocks the running system has (/etc/burrow/house). A pass whose
    /// `house` isn't these is from before a switch, and does nothing.
    pub current: PathBuf,
    /// Held by every pass that can change anything, one at a time.
    pub lock: PathBuf,
    /// The GitHub users and orgs whose repos it keeps.
    pub owners: Vec<String>,
    /// Repos it leaves alone (exclude.rs).
    pub exclude: Vec<Exclude>,
    /// HOST-notify: `NOTIFY open|close KEY TITLE [BODY]`.
    pub notify: PathBuf,
    /// Only say what a pass would do, as `status` does.
    pub dry_run: bool,
}

impl Config {
    pub fn read(path: &Path) -> Result<Config, String> {
        let text = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_slice(&text).map_err(|e| format!("{}: {e}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_only_what_it_knows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut config = serde_json::json!({
            "house": "/h", "current": "/c", "lock": "/l", "owners": ["sam"],
            "exclude": ["sam/*"], "notify": "/n", "dryRun": false,
        });
        std::fs::write(&path, config.to_string()).unwrap();
        let read = Config::read(&path).unwrap();
        assert_eq!(
            (read.owners, read.dry_run),
            (vec!["sam".to_string()], false)
        );
        config["dry_run"] = true.into();
        std::fs::write(&path, config.to_string()).unwrap();
        let e = Config::read(&path).unwrap_err();
        assert!(e.contains("unknown field `dry_run`"), "{e}");
        assert!(Config::read(&dir.path().join("missing")).is_err());
        // An exclude entry it can't read as OWNER/NAME is refused with it.
        config.as_object_mut().unwrap().remove("dry_run");
        config["exclude"] = serde_json::json!(["sam/*", "sam/[ab]"]);
        std::fs::write(&path, config.to_string()).unwrap();
        let e = Config::read(&path).unwrap_err();
        assert!(
            e.contains(r#"exclude entry "sam/[ab]" isn't OWNER/NAME"#),
            "{e}"
        );
    }
}
