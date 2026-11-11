//! The engines door shares proj's checked switch; issue text is never a command.
use burrow_door::{Door, asked, fenced};
use burrow_github::{Api, Error};
use burrow_proj::setups::{Store, valid_name};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::path::Path;
use std::path::PathBuf;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub repo: String,
    pub label: String,
    pub authors: Vec<String>,
    pub roles: PathBuf,
    pub state: PathBuf,
}

/// A completed request whose GitHub answer is still pending. Keeping it until
/// the close succeeds makes a retry of `use back` a reply, never another switch.
/// Receipts stay keyed by open cycle: a lost close response or cleanup cannot
/// turn a reopened issue into a retry of its previous request.
#[derive(Serialize, Deserialize)]
struct Pending {
    body: String,
    reason: String,
}

fn stored<T>(path: &Path, result: std::io::Result<T>) -> Result<T, String> {
    result.map_err(|e| format!("{}: {e}", path.display()))
}

pub fn pass(api: &mut dyn Api, cfg: &Config) -> Result<(), Error> {
    let door = Door {
        repo: &cfg.repo,
        label: &cfg.label,
        authors: &cfg.authors,
        what: "this machine's engines",
    };
    // A pass is serialized separately from the switch's CLI/door lock.
    // No issue text is used in these paths: the configured repo and GitHub's
    // numeric issue number alone name a pending result.
    if !burrow_github::repo_name(&cfg.repo) {
        return Err("engines repo must be OWNER/NAME".into());
    }
    let state = cfg
        .state
        .join(cfg.repo.to_ascii_lowercase().replace('/', "--"));
    stored(&state, fs::create_dir_all(&state))?;
    let lock_path = state.join("lock");
    let lock = stored(
        &lock_path,
        File::options()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path),
    )?;
    stored(&lock_path, lock.lock())?;
    for issue in door.issues(api)? {
        // A non-author's text is ignored, including edits by outsiders.
        let (text, cycle) = match door.written_cycle(api, &issue)? {
            Ok(t) => t,
            Err(_) => continue,
        };
        let cycle: String = Sha256::digest(cycle.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let receipt = state.join(format!("{}-{cycle}.json", issue.number));
        let operation = receipt.with_extension("operation");
        let pending = match fs::read(&receipt) {
            Ok(bytes) => Some(
                serde_json::from_slice::<Pending>(&bytes)
                    .map_err(|e| format!("{}: {e}", receipt.display()))?,
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(format!("{}: {e}", receipt.display()).into()),
        };
        if let Some(pending) = pending {
            door.reply(api, issue.number, &pending.body, &pending.reason)?;
            continue;
        }
        let lines = asked(&text);
        let args = match lines.as_slice() {
            [line] if line == "list" => Some(vec!["list".into()]),
            [line] => match line.split_whitespace().collect::<Vec<_>>().as_slice() {
                ["use", name] if valid_name(name) => Some(vec!["use".into(), (*name).into()]),
                _ => None,
            },
            _ => None,
        };
        let store = Store::new(&cfg.roles);
        let (body, reason) = match args {
            Some(args) => match if args[0] == "use" {
                store.request(&operation, &args)
            } else {
                store.command(&args)
            } {
                Ok(said) => (fenced(&said), "completed"),
                // Roles errors include the raw line, which may name a model.
                // Keep just the location, never echo issue text or model choices.
                Err(e) => {
                    if store.pending()? || operation.try_exists().map_err(|e| e.to_string())? {
                        return Err(e.into());
                    }
                    let why = diagnostic(&e);
                    let current = store
                        .command(&["list".into()])
                        .map(|s| fenced(&s))
                        .unwrap_or_else(|e| diagnostic(&e));
                    (
                        format!("Nothing changed. {why}\n\n{current}"),
                        "not_planned",
                    )
                }
            },
            None => {
                let current = store
                    .command(&["list".into()])
                    .map(|s| fenced(&s))
                    .unwrap_or_else(|e| diagnostic(&e));
                (
                    format!(
                        "Nothing changed. Ask one line: `list`, `use NAME` or `use back`.\n\n{current}"
                    ),
                    "not_planned",
                )
            }
        };
        let pending = Pending {
            body,
            reason: reason.into(),
        };
        let tmp = receipt.with_extension("next");
        let bytes = serde_json::to_vec(&pending).map_err(|e| e.to_string())?;
        stored(&tmp, fs::write(&tmp, bytes))?;
        stored(&receipt, fs::rename(&tmp, &receipt))?;
        door.reply(api, issue.number, &pending.body, reason)?;
    }
    Ok(())
}

/// The location and class of an error, with no raw roles line or model choice.
fn diagnostic(error: &str) -> String {
    let location = error.split(": ").next().unwrap_or("");
    if location.starts_with('/') {
        let line = error
            .split("line ")
            .nth(1)
            .and_then(|s| s.split(|c: char| !c.is_ascii_digit()).next())
            .filter(|s| !s.is_empty());
        if !location.contains(" line ")
            && let Some(line) = line
        {
            return format!("Invalid configuration: {location} line {line}.");
        }
        return format!("Check {location}.");
    }
    if error.starts_with("unknown setup:") {
        "Unknown setup.".into()
    } else if error == "no previous setup" {
        "No previous setup.".into()
    } else {
        "Configuration or switch failed.".into()
    }
}

#[cfg(test)]
mod tests;
