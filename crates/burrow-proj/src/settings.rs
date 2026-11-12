//! `proj settings`: the read door of burrow-settings' schema. Each option
//! is read from where it lives today: the flake's, from the values
//! modules/settings/tracked.nix renders into the file proj's config names; the live ones,
//! from proj's own files, through the parsers its turns use. The schema
//! decides what may be shown ([`burrow_settings::show::Document`]).

use std::path::Path;

use burrow_settings::show::{self, Document, Reading};
use serde_json::{Value, json};

use crate::config::Config;
use crate::usage;

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
    let (file, overrides, until) = if let Some(policy) = &cfg.settings {
        let (overrides, until) = policy.readings()?;
        (
            policy.overlay.clone(),
            there(&policy.overlay).then(|| json!(overrides)),
            there(&policy.overlay).then(|| json!(until)),
        )
    } else {
        (cfg.jobs.join(".settings-unconfigured"), None, None)
    };
    readings.push(Reading::live(
        "settings.repositoryOverrides",
        file.clone(),
        overrides,
    ));
    readings.push(Reading::live("settings.pipelineUntil", file.clone(), until));
    let (engines, owner) = cfg
        .settings
        .as_ref()
        .map(|s| s.engine_snapshot())
        .transpose()?
        .unwrap_or_else(|| (Default::default(), cfg.jobs.join(".settings-unconfigured")));
    let set = !engines.is_empty();
    let tuning =
        |t: &burrow_settings::engines::Tuning| json!({"model": t.model, "effort": t.effort});
    let models: serde_json::Map<String, Value> = engines
        .models
        .iter()
        .map(|(k, t)| (k.clone(), tuning(t)))
        .collect();
    let saved: serde_json::Map<String, Value> = engines
        .saved
        .iter()
        .map(|(n, s)| {
            let m: serde_json::Map<String, Value> = s
                .models
                .iter()
                .map(|(k, t)| (k.clone(), tuning(t)))
                .collect();
            (n.clone(), json!({"roles": s.roles, "models": m}))
        })
        .collect();
    let when = |v: Value, on: bool| on.then_some(v);
    readings.push(Reading::live(
        "proj.roles",
        owner.clone(),
        when(json!(engines.roles), set),
    ));
    readings.push(Reading::live(
        "proj.models",
        owner.clone(),
        when(Value::Object(models), set),
    ));
    readings.push(Reading::live(
        "proj.offeredModels",
        owner.clone(),
        when(json!(engines.offered), set),
    ));
    readings.push(Reading::live(
        "proj.engineSetups",
        owner,
        when(
            json!({"active": engines.active(), "previous": engines.previous, "saved": saved}),
            set,
        ),
    ));
    readings.push(Reading::live(
        "proj.usageHold",
        cfg.jobs.join(usage::FILE),
        usage::held(cfg)?
            .map(|s| json!({"until": s.until, "since": s.at, "owner": s.owner, "reason": s.said})),
    ));
    Document::build(readings)
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
