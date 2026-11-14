//! What modules/escalation.nix tells it.

use std::fs;
use std::path::{Path, PathBuf};

use burrow_github::Author;
use serde::Deserialize;

use crate::reason::{Kind, Policy};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Config {
    /// Its command's name (HOST-escalation).
    pub name: String,
    /// Who the owner is, as the requests name them.
    pub owner: String,
    /// The label that says an issue or PR waits on the owner.
    pub owner_label: String,
    /// The reasons that may reach the owner (`escalation.ownerReasons`).
    pub owner_reasons: Vec<String>,
    /// The route to the owner's project; none, and nothing reaches them.
    pub relay: Option<Relay>,
    /// One record per escalation, kept until it's done.
    pub dir: PathBuf,
    /// The user whose gh reaches GitHub, and their home.
    pub user: String,
    pub home: PathBuf,
}

/// The coordinator's path: a request issue in `repo` under `label`, which
/// counts as delivered only once one of `acknowledgers` says so on it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Relay {
    pub repo: String,
    pub label: String,
    pub acknowledgers: Vec<Author>,
}

impl Config {
    pub fn read(path: &Path) -> Result<Config, String> {
        let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let cfg: Config =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        cfg.policy()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if cfg.owner_label.trim().is_empty() {
            return Err(format!("{}: ownerLabel is empty", path.display()));
        }
        if let Some(relay) = &cfg.relay {
            if relay.label.trim().is_empty() || !relay.repo.contains('/') {
                return Err(format!(
                    "{}: the relay needs OWNER/NAME and a label",
                    path.display()
                ));
            }
            // An acknowledgement only counts from an account the machine
            // trusts by its numeric id; with none, nothing could ever be
            // delivered.
            if relay.acknowledgers.is_empty() || relay.acknowledgers.iter().any(|a| a.id == 0) {
                return Err(format!(
                    "{}: the relay needs acknowledgers, each with its account id",
                    path.display()
                ));
            }
        }
        Ok(cfg)
    }

    /// The policy its values make; an unknown reason fails it, so a typo
    /// can't quietly take a door away from the owner.
    pub fn policy(&self) -> Result<Policy, String> {
        let owner_reasons = self
            .owner_reasons
            .iter()
            .map(|w| Kind::parse(w).ok_or_else(|| format!("{w:?} isn't an owner reason")))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Policy {
            owner_reasons,
            delivers: self.relay.is_some(),
        })
    }
}
