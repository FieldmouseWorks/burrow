//! The engines door is a caller of the settings door: `engines use NAME`
//! there, with a receipt for the issue's open cycle. Issue text is never a
//! command.
use burrow_door::{Door, asked, fenced};
use burrow_github::{Api, Error};
use burrow_settings::roles::valid_setup;
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
    pub authors: Vec<burrow_github::Author>,
    /// The settings the setups are kept in, and switched through.
    pub settings: burrow_settings::Config,
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

/// The receipt the door before the settings left for a switch it made
/// (proj's `.operation` file): only its answer matters now.
#[derive(Deserialize)]
struct Legacy {
    result: Option<String>,
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
    let lock_path = state.join("settings.lock");
    let lock = stored(
        &lock_path,
        File::options()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path),
    )?;
    stored(&lock_path, lock.lock())?;
    // Migration also fences the legacy pass lock. Never answer a legacy
    // receipt while its recovered switch is still waiting for publication.
    cfg.settings.roles()?;
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
        // A request the door before the settings completed, its answer
        // still to post: answered again, never made again.
        let legacy = match cfg.settings.legacy_answer(&operation, None)? {
            Some(said) => Some(said),
            None => match fs::read(&operation) {
                Ok(bytes) => serde_json::from_slice::<Legacy>(&bytes)
                    .map_err(|e| format!("{}: {e}", operation.display()))?
                    .result
                    .map(|said| cfg.settings.legacy_answer(&operation, Some(&said)))
                    .transpose()?
                    .flatten(),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(format!("{}: {e}", operation.display()).into()),
            },
        };
        let lines = asked(&text);
        let args = match lines.as_slice() {
            [line] if line == "list" => Some(vec!["engines".into(), "list".into()]),
            [line] => match line.split_whitespace().collect::<Vec<_>>().as_slice() {
                ["use", name] if *name == "back" || valid_setup(name) => {
                    Some(vec!["engines".into(), "use".into(), (*name).into()])
                }
                _ => None,
            },
            _ => None,
        };
        let current = || {
            cfg.settings
                .engines()
                .map(|s| fenced(&s))
                .unwrap_or_else(|e| diagnostic(&e))
        };
        let (body, reason) = match (legacy, args) {
            (Some(said), _) => (fenced(&said), "completed"),
            (None, args) => match args {
                // The receipt is the overlay's: a retried delivery of this open
                // cycle's request answers again, never switches again.
                Some(args) => match cfg.settings.request(
                    &operation,
                    &args,
                    &format!("github:{}#{}:{}", cfg.repo, issue.number, issue.opener),
                ) {
                    Ok(said) => (fenced(&said), "completed"),
                    // Published, though a step after it failed, so maybe not on
                    // disk: no answer yet. The next pass's retry finds its
                    // receipt, makes it durable and answers then.
                    Err(e) if cfg.settings.answered(&operation)?.is_some() => {
                        return Err(e.into());
                    }
                    Err(e) => (
                        format!("Nothing changed. {}\n\n{}", diagnostic(&e), current()),
                        "not_planned",
                    ),
                },
                None => (
                    format!(
                        "Nothing changed. Ask one line: `list`, `use NAME` or `use back`.\n\n{}",
                        current()
                    ),
                    "not_planned",
                ),
            },
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

/// The class of an error, with no model id, which a refusal can't name.
fn diagnostic(error: &str) -> String {
    if error.starts_with("unknown setup:") {
        "Unknown setup.".into()
    } else if error == "no previous setup" {
        "No previous setup.".into()
    } else if error.contains("doesn't offer") {
        "The setup names a model or effort its engine doesn't offer now.".into()
    } else {
        "Configuration or switch failed.".into()
    }
}

#[cfg(test)]
mod tests;
