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
    let (file, overrides, until) = match &cfg.settings {
        Some(policy) if burrow_settings::path_exists(&policy.overlay)? => {
            let (overrides, until) = policy.readings()?;
            (
                policy.overlay.clone(),
                Some(json!(overrides)),
                Some(json!(until)),
            )
        }
        Some(policy) => (policy.overlay.clone(), None, None),
        None => (cfg.jobs.join(".settings-unconfigured"), None, None),
    };
    readings.push(Reading::live(
        "settings.repositoryOverrides",
        file.clone(),
        overrides,
    ));
    readings.push(Reading::live("settings.pipelineUntil", file, until));
    // Each role setting, from one version of them: the declared defaults
    // with what's set over them, or the default where nothing is.
    let view = cfg.role_view()?;
    let (roles, models, offers) = view.set;
    let force = &view.in_force;
    for (name, set, value) in [
        ("proj.roles", roles, json!(force.roles)),
        ("proj.models", models, json!(force.models)),
        ("proj.engineOffers", offers, json!(force.offers)),
    ] {
        readings.push(Reading::declared(name, view.file.clone(), set, value));
    }
    readings.push(Reading::live(
        "proj.engineSetups",
        view.file.clone(),
        view.setups,
    ));
    readings.push(Reading::live(
        "proj.usageHold",
        cfg.jobs.join(usage::FILE),
        usage::held(cfg)?
            .map(|s| json!({"until": s.until, "since": s.at, "owner": s.owner, "reason": s.said})),
    ));
    Document::build(readings)
}

/// The first of proj's old role files in DIR, which only `proj settings
/// migrate` reads.
pub(crate) fn old_files(dir: &Path) -> Result<Option<std::path::PathBuf>, String> {
    burrow_settings::roles::legacy_file(dir)
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
        ["_migration-ready"] => {
            let settings = cfg.settings.as_ref().ok_or("settings are not configured")?;
            if !settings.roles_migrated()? {
                return Err("role settings migration has not completed".into());
            }
            settings.migrate(
                burrow_settings::roles::Migration::default(),
                "migration health",
            )?;
            Ok("Role settings migration is durable.\n".into())
        }
        ["migrate", "--fence"] => migrate::fenced(
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
