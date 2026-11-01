//! The age: the longest minimumReleaseAge the machine's Renovate config,
//! its globalExtends and the repo's own config set, as release-ages reads
//! them.

use std::path::Path;

use jiff::SignedDuration;
use serde_json::{Value, json};

use crate::{Ages, World, git};

/// Where Renovate looks for a repo's own config, in its order
/// (config/app-strings.ts; it doesn't filter by platform there), after any
/// configFileNames the machine adds.
const CONFIGS: [&str; 14] = [
    "renovate.json",
    "renovate.jsonc",
    "renovate.json5",
    ".github/renovate.json",
    ".github/renovate.jsonc",
    ".github/renovate.json5",
    ".gitlab/renovate.json",
    ".gitlab/renovate.jsonc",
    ".gitlab/renovate.json5",
    ".renovaterc",
    ".renovaterc.json",
    ".renovaterc.jsonc",
    ".renovaterc.json5",
    "package.json",
];

/// The age the refresh is held to, from the machine's `policy` and the
/// base's own config.
pub fn age(dir: &Path, policy: &Value, w: &mut dyn World) -> Result<SignedDuration, String> {
    let glob = w.release_ages(
        &json!({"extends": policy.get("globalExtends").cloned().unwrap_or(json!([]))}),
    )?;
    let machine = w.release_ages(policy)?;
    if glob.inherit_config || machine.inherit_config {
        return Err(
            "the machine's Renovate config inherits config from another repo, which could set \
                    a release age lock-age can't see"
                .into(),
        );
    }
    if glob.fork_mode || machine.fork_mode {
        return Err(
            "the machine's Renovate config works in forks, whose default branch needn't be \
                    the one Renovate reads its config from"
                .into(),
        );
    }
    // Renovate settles which files to look in before it resolves the
    // machine's own extends: from the machine's config as it's written,
    // over its globalExtends'. Set to null, only Renovate's own.
    let files = match policy.get("configFileNames") {
        Some(v) => Some(v.clone()),
        None => glob.config_file_names.clone(),
    };
    let files: Vec<String> = match files {
        None | Some(Value::Null) => vec![],
        Some(Value::Array(a)) => a
            .iter()
            .map(|x| {
                x.as_str()
                    .map(String::from)
                    .ok_or("configFileNames: not a list of names")
            })
            .collect::<Result<_, _>>()?,
        Some(_) => return Err("configFileNames: not a list of names".into()),
    };
    let repo = match repo_config(dir, &files)? {
        Some(c) => Some(w.release_ages(&c)?),
        None => None,
    };
    let ms = [Some(&glob), Some(&machine), repo.as_ref()]
        .into_iter()
        .flatten()
        .flat_map(|a: &Ages| a.ages.iter().copied())
        .fold(0.0, f64::max);
    SignedDuration::try_from_secs_f64(ms / 1000.0).map_err(|e| format!("the age: {e}"))
}

/// The base's own Renovate config, or None. Renovate reads only the first
/// config file it finds, and package.json only for its "renovate" key.
fn repo_config(dir: &Path, files: &[String]) -> Result<Option<Value>, String> {
    for name in files.iter().map(String::as_str).chain(CONFIGS) {
        let Some(text) = git::base_text(dir, name) else {
            continue;
        };
        let mut config: Value = json5::from_str(&text).map_err(|e| format!("{name}: {e}"))?;
        if name == "package.json" {
            match config.get_mut("renovate") {
                Some(c) => config = c.take(),
                None => continue,
            }
        }
        if !config.is_object() {
            return Err(format!("{name}: not a Renovate config"));
        }
        return Ok(Some(config));
    }
    Ok(None)
}
