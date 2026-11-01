//! heavy's config, as modules/cpu.nix writes it: the `heavy` wrapper passes
//! its path as the first argument.

use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// How many heavy runs the machine takes at once (burrow.cpu.heavy.slots).
    pub slots: u32,
    /// Seconds a cancelled run gets to stop before what's left of it is
    /// killed.
    pub grace: u64,
    /// Where the slots' lock files are, `slot1` to `slotN`.
    pub dir: PathBuf,
    /// util-linux's setsid, which starts the command in a session of its own.
    pub setsid: PathBuf,
}

impl Config {
    pub fn load(path: &Path) -> Result<Config, String> {
        let text = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let config: Config =
            serde_json::from_slice(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        if config.slots == 0 {
            return Err(format!("{}: slots is 0", path.display()));
        }
        Ok(config)
    }

    /// Slot `i`'s lock file, from 1.
    pub fn slot(&self, i: u32) -> PathBuf {
        self.dir.join(format!("slot{i}"))
    }
}
