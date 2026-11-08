//! The engines door shares proj's checked switch; issue text is never a command.
use burrow_door::{Door, asked, fenced};
use burrow_github::Api;
use burrow_proj::setups::{Store, valid_name};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub repo: String,
    pub label: String,
    pub authors: Vec<String>,
    pub roles: PathBuf,
}

pub fn pass(api: &mut dyn Api, cfg: &Config) -> Result<(), String> {
    let door = Door {
        repo: &cfg.repo,
        label: &cfg.label,
        authors: &cfg.authors,
        what: "this machine's engines",
    };
    for issue in door.issues(api)? {
        // A non-author's text is ignored, including edits by outsiders.
        let text = match door.written(api, &issue)? {
            Ok(t) => t,
            Err(_) => continue,
        };
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
            Some(args) => match store.command(&args) {
                Ok(said) => (fenced(&said), "completed"),
                // Roles errors include the raw line, which may name a model.
                // Keep just the location, never echo issue text or model choices.
                Err(e) => {
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
        door.reply(api, issue.number, &body, reason)?;
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
