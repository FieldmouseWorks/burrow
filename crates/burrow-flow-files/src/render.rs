//! `burrow-flow-files render MANIFEST`: each repo's flow config as the flow
//! reads it, its base (what the machine's build rendered and held to
//! Conary's loader, at BASE/OWNER/NAME.json) with what each plugin on a
//! role is asked to run with (its `execution`) as the settings say now, at
//! OUT/OWNER/NAME.json.
//!
//! A build reads no live settings, and model ids live only in the role
//! overlay, so a choice a build rendered names no model, which every job
//! refuses (`burrow_producer::jobs::chosen`, burrow#1393). So the choices
//! are rendered here, on the machine, whenever the role settings or the
//! base change (autoland/shadow.nix), and conaryd, which reads its flow
//! config at each pass, runs the next one on them with no deploy.
//!
//! All or nothing: a role whose choice would be refused (no model, or one
//! its engine doesn't offer) fails the render and writes nothing, so the
//! last good configs stay as they were and the failed unit says why. Each
//! file is replaced whole (`burrow_settings::publish`), and a repo's no
//! longer in the base is removed.
use burrow_producer::jobs;
use conary_core::plugin::Choice;
use conary_github::flow::Config;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// What the machine's build gives the renderer.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// The settings source (`burrow_settings::Config`), whose role overlay
    /// is read as the render runs.
    pub settings: PathBuf,
    /// The configs as the build rendered them: BASE/OWNER/NAME.json.
    pub base: PathBuf,
    /// Where the flow and its plugins read them: OUT/OWNER/NAME.json.
    pub out: PathBuf,
    /// Each plugin kind whose choice is a role's, by that role. A kind
    /// whose choice is its own (the proof plugin's) is in the base.
    pub roles: BTreeMap<String, String>,
}

/// How many times a render goes again when the role settings changed
/// under it, before it gives up: a change that comes as it runs starts no
/// render of its own.
const TRIES: usize = 5;

/// Render every repo's config, as the settings are once they hold still
/// across a whole render: how many it wrote.
pub fn render(manifest: &Manifest) -> Result<usize, String> {
    let settings = read_settings(&manifest.settings)?;
    let roles_file = settings.roles_file();
    for _ in 0..TRIES {
        let before = fs::read(&roles_file).ok();
        let written = render_once(manifest, &settings)?;
        if fs::read(&roles_file).ok() == before {
            return Ok(written);
        }
    }
    Err(format!(
        "{} kept changing while the flow configs were rendered",
        roles_file.display()
    ))
}

fn read_settings(path: &Path) -> Result<burrow_settings::Config, String> {
    let bytes = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}

fn render_once(manifest: &Manifest, settings: &burrow_settings::Config) -> Result<usize, String> {
    let choices = choices(settings, &manifest.roles)?;
    // Everything is rendered and checked before anything is written.
    let mut files = Vec::new();
    for (name, base) in bases(&manifest.base)? {
        let body = with_execution(&base, &choices).map_err(|e| format!("{name}: {e}"))?;
        files.push((name, body));
    }
    for (name, body) in &files {
        let path = manifest.out.join(name);
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        burrow_settings::publish(&path, body).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    let kept: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
    prune(&manifest.out, &kept)?;
    Ok(files.len())
}

/// Each role-run kind's choice as the settings say now, refused unless it
/// names a model the role's engine offers: what a job on it would be held
/// to.
pub fn choices(
    settings: &burrow_settings::Config,
    roles: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, Choice>, String> {
    roles
        .iter()
        .map(|(kind, role)| {
            let choice = jobs::choice(settings, role).map_err(|e| format!("{kind}: {e}"))?;
            jobs::chosen(Some(&choice)).map_err(|e| {
                format!("{kind}: the {role} role names no model on {} (`proj settings model {role} {} MODEL`): {e}", choice.engine, choice.engine)
            })?;
            Ok((kind.clone(), choice))
        })
        .collect()
}

/// A base config with `choices` in its `execution`, beside what it had
/// there (a plugin's own), held to Conary's schema.
pub fn with_execution(base: &[u8], choices: &BTreeMap<String, Choice>) -> Result<Vec<u8>, String> {
    let mut config: serde_json::Value = serde_json::from_slice(base).map_err(|e| e.to_string())?;
    let object = config.as_object_mut().ok_or("a flow config is an object")?;
    if !choices.is_empty() {
        let execution = object
            .entry("execution")
            .or_insert_with(|| serde_json::json!({}))
            .as_object_mut()
            .ok_or("its execution isn't an object")?;
        for (kind, choice) in choices {
            execution.insert(kind.clone(), serde_json::json!(choice));
        }
    }
    let body = serde_json::to_vec(&config).map_err(|e| e.to_string())?;
    conary_github::flow::schema::load::<Config>(&body).map_err(|e| e.to_string())?;
    Ok(body)
}

/// The base's configs, by OWNER/NAME.json.
fn bases(base: &Path) -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut found = Vec::new();
    for (owner, file) in entries(base)? {
        let name = format!("{owner}/{file}");
        let path = base.join(&name);
        let bytes = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        found.push((name, bytes));
    }
    Ok(found)
}

/// The OWNER/NAME.json files under `dir`, none if it isn't there.
fn entries(dir: &Path) -> Result<Vec<(String, String)>, String> {
    let names = |d: &Path| -> Result<Vec<String>, String> {
        match fs::read_dir(d) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(format!("{}: {e}", d.display())),
            Ok(it) => {
                let mut v = Vec::new();
                for entry in it {
                    let entry = entry.map_err(|e| format!("{}: {e}", d.display()))?;
                    v.push(entry.file_name().to_string_lossy().into_owned());
                }
                v.sort();
                Ok(v)
            }
        }
    };
    let mut found = Vec::new();
    for owner in names(dir)? {
        let sub = dir.join(&owner);
        if !sub.is_dir() {
            continue;
        }
        for file in names(&sub)? {
            if file.ends_with(".json") {
                found.push((owner.clone(), file));
            }
        }
    }
    Ok(found)
}

/// Remove the configs under `out` that aren't `kept`, and the owners left
/// with none: a repo taken out of the base has no config for anyone to
/// read.
fn prune(out: &Path, kept: &[&str]) -> Result<(), String> {
    for (owner, file) in entries(out)? {
        let name = format!("{owner}/{file}");
        if !kept.contains(&name.as_str()) {
            let path = out.join(&name);
            fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        }
        // Gone only once empty.
        let _ = fs::remove_dir(out.join(&owner));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
