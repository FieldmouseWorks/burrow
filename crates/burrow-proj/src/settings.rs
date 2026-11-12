//! `proj settings`: the read door of burrow-settings' schema. Each option
//! is read from where it lives today: the flake's, from the values
//! modules/settings/tracked.nix renders into the file proj's config names;
//! the live ones, from the settings overlay (and proj's usage record), as
//! its turns read them. The schema decides what may be shown
//! ([`burrow_settings::show::Document`]).

use std::path::Path;

use burrow_settings::show::{self, Document, Reading};
use serde_json::{Value, json};

use crate::config::Config;
use crate::usage;

mod migrate;

/// Every option in effect, and where each came from.
pub fn document(cfg: &Config) -> Result<Document, String> {
    let file = cfg
        .tracked_settings
        .as_deref()
        .ok_or("proj's config names no settings file")?;
    let text = std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let tracked: serde_json::Map<String, Value> =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", file.display()))?;
    let mut readings = show::flake(&tracked)?;
    let nothing = (false, false, false);
    let (file, overrides, until, set, setups) = match &cfg.settings {
        Some(policy) if there(&policy.overlay) => {
            let (overrides, until) = policy.readings()?;
            (
                policy.overlay.clone(),
                Some(json!(overrides)),
                Some(json!(until)),
                policy.roles_set()?,
                policy.setups()?,
            )
        }
        Some(policy) => (policy.overlay.clone(), None, None, nothing, None),
        None => (
            cfg.jobs.join(".settings-unconfigured"),
            None,
            None,
            nothing,
            None,
        ),
    };
    readings.push(Reading::live(
        "settings.repositoryOverrides",
        file.clone(),
        overrides,
    ));
    readings.push(Reading::live("settings.pipelineUntil", file.clone(), until));
    // Each role setting in force, the declared defaults with what the
    // overlay sets over them; the default where it sets nothing.
    let force = cfg.role_settings()?;
    let (roles, models, offers) = set;
    readings.push(Reading::live(
        "proj.roles",
        file.clone(),
        roles.then(|| json!(force.roles)),
    ));
    readings.push(Reading::live(
        "proj.models",
        file.clone(),
        models.then(|| json!(force.models)),
    ));
    readings.push(Reading::live(
        "proj.engineOffers",
        file.clone(),
        offers.then(|| json!(force.offers)),
    ));
    readings.push(Reading::live("proj.engineSetups", file, setups));
    readings.push(Reading::live(
        "proj.usageHold",
        cfg.jobs.join(usage::FILE),
        usage::held(cfg)?
            .map(|s| json!({"until": s.until, "since": s.at, "owner": s.owner, "reason": s.said})),
    ));
    Document::build(readings)
}

/// proj's old roles and models files still in DIR, which nothing reads.
pub(crate) fn old_files(dir: &Path) -> Vec<std::path::PathBuf> {
    migrate::left(dir)
}

/// Whether a live file is there to say anything: one that isn't leaves the
/// default.
fn there(path: &Path) -> bool {
    path.symlink_metadata().is_ok()
}

/// `proj settings show [--json]` and `proj settings schema`.
pub fn command(cfg: &Config, args: &[String]) -> Result<String, String> {
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [] | ["show"] => Ok(document(cfg)?.text()),
        ["show", "--json"] => Ok(document(cfg)?.json()),
        ["migrate"] => migrate::run(
            cfg.settings.as_ref().ok_or("settings are not configured")?,
            cfg.old_config
                .as_deref()
                .ok_or("proj's config names no old config folder")?,
        ),
        ["schema"] => Ok(
            serde_json::to_string_pretty(&burrow_settings::schema::json_schema())
                .expect("a schema is JSON")
                + "\n",
        ),
        _ => cfg
            .settings
            .as_ref()
            .ok_or("settings are not configured")?
            .command(args, "proj"),
    }
}

#[cfg(test)]
mod tests;
