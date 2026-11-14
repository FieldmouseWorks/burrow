//! The `first` label (burrow.autoland.labels.first) on an issue or PR puts
//! its work ahead of the line, and counts only when one of the owners put it
//! on, as GitHub names the account that did. A Claude job for a person's
//! issue acts on GitHub as that person, so GitHub can't tell its label from
//! theirs: here a proj job's commands that would put the label on are
//! refused (asked about, which a job can't answer), whoever the job acts
//! as, so no job moves work up the line, its own or any other.
//!
//! What puts it on: `gh issue|pr edit --add-label`, `gh issue|pr create
//! --label`, and a write through GitHub's API, `gh api`'s or curl's, at an
//! issue, its labels, or a repo's issues (a new one's labels), whose labels
//! name it. What the guard can't read there counts as naming it: a label,
//! an endpoint, a labels field or a body only an expansion or a file gives,
//! and GraphQL's label writes, which name labels by id. Elsewhere an
//! expansion is left alone, so a job still posts a comment from a variable.
//! It reads a command as a job means it; a program that writes the label
//! itself isn't a command it sees.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::codex::{endpoint, program};
use crate::curl::{self, Curl};
use crate::gh::{self, Api, expands};
use crate::{pattern, walk};

/// Why.
fn reason(label: &str) -> String {
    format!(
        "Puts the `{label}` label on an issue or PR, or may (a label, endpoint or body the guard can't read, or GraphQL's label ids). It counts only when the owner puts it on, and this job would put it on as them or as an App: no job moves work up the line, its own or any other. Leave it to the owner, or name the labels you mean on the line."
    )
}

/// Where GitHub's REST API puts labels on: an issue (or PR, which is one)
/// with `labels`, its labels, or a new issue. An endpoint whose last
/// segment the guard can't read may be any of them.
static LABELLED: LazyLock<Regex> =
    LazyLock::new(|| pattern(r"(^|/)issues(/[^/]+(/labels)?)?$|[$\x{FFFC}][^/]*$"));

/// GitHub's GraphQL label writes, which name labels by id.
static LABEL_IDS: LazyLock<Regex> = LazyLock::new(|| pattern(r"addLabelsToLabelable|labelIds"));

/// Whether a list of label names, `,`-separated as gh takes them, names
/// `label` (GitHub matches names whatever their case), or may.
fn names(list: &str, label: &str) -> bool {
    expands(list)
        || list
            .split(',')
            .any(|l| l.trim().eq_ignore_ascii_case(label))
}

/// Whether a request body (JSON, or `key=value&…`) gives `labels` that
/// name `label`. One the guard can read neither way may.
fn body_names(body: &str, label: &str) -> bool {
    let named = |v: &Value| match v {
        Value::String(s) => s.eq_ignore_ascii_case(label),
        Value::Object(o) => o
            .get("name")
            .and_then(Value::as_str)
            .is_some_and(|s| s.eq_ignore_ascii_case(label)),
        _ => false,
    };
    let in_list = |v: Option<&Value>| match v {
        Some(Value::Array(a)) => a.iter().any(named),
        Some(v) => named(v),
        None => false,
    };
    match serde_json::from_str::<Value>(body) {
        Ok(Value::Array(a)) => a.iter().any(named),
        Ok(v @ Value::Object(_)) => in_list(v.get("labels")),
        Ok(_) => false,
        Err(_) if body.contains('=') => body.split('&').any(|pair| {
            pair.split_once('=').is_some_and(|(k, v)| {
                k.to_ascii_lowercase().starts_with("labels") && names(v, label)
            })
        }),
        Err(_) => body
            .to_ascii_lowercase()
            .contains(&label.to_ascii_lowercase()),
    }
}

/// Whether a write to GitHub's API (curl.rs's [`curl::Counts`]) puts
/// `label` on, or may.
fn writes_label(label: &str, endpoint: Option<&str>, body: &[&str], unread_body: bool) -> bool {
    match endpoint {
        Some(path) if path.ends_with("graphql") => {
            unread_body || body.iter().any(|b| LABEL_IDS.is_match(b))
        }
        Some(path) if !LABELLED.is_match(path) => false,
        _ => unread_body || body.iter().any(|b| body_names(b, label)),
    }
}

/// Whether `gh api`'s words put `label` on, or may.
fn gh_api(words: &[&str], label: &str) -> bool {
    let api = Api::read(words);
    if !api.writes() {
        return false;
    }
    // A field's own key and value, as gh sends them: `labels[]=first`.
    let field_names = |f: &gh::Field<'_>| {
        let (key, value) = f.text.split_once('=').unwrap_or((f.text, ""));
        expands(key)
            || (key.to_ascii_lowercase().starts_with("labels")
                && (names(value, label) || (f.typed && value.starts_with('@'))))
    };
    api.urls.iter().any(|url| {
        let path = endpoint(url);
        if path.ends_with("graphql") {
            api.unread_query() || api.fields.iter().any(|f| LABEL_IDS.is_match(f.text))
        } else {
            LABELLED.is_match(path) && (api.input || api.fields.iter().any(field_names))
        }
    })
}

/// Whether `gh`'s words put `label` on, or may.
fn gh<'a>(words: impl Iterator<Item = &'a String>, label: &str) -> bool {
    let gh = gh::read(words);
    let flag = match gh.command.as_slice() {
        ["api"] => return gh_api(&gh.rest, label),
        ["issue" | "pr", "edit"] => ("--add-label", None),
        ["issue" | "pr", "create"] => ("--label", Some("-l")),
        _ => return false,
    };
    let mut words = gh.rest.iter().copied();
    while let Some(w) = words.next() {
        if w == "--" {
            break;
        }
        let value = if w == flag.0 || Some(w) == flag.1 {
            words.next()
        } else if let Some(v) = w.strip_prefix(flag.0).and_then(|v| v.strip_prefix('=')) {
            Some(v)
        } else {
            flag.1
                .and_then(|short| w.strip_prefix(short))
                .filter(|v| !v.is_empty())
        };
        if value.is_some_and(|v| names(v, label)) {
            return true;
        }
    }
    false
}

/// The refusal for a command of a job's that puts `label` on, if it does.
pub fn refusals(cmd: &str, label: &str) -> Vec<String> {
    let counts = |endpoint: Option<&str>, body: &[&str], unread: bool| {
        writes_label(label, endpoint, body, unread)
    };
    let mut found = false;
    // A command too long or deep to read is push.rs's question.
    let _ = walk::walk(cmd, &mut |word, rest, budget, _more| {
        found |= match program(word) {
            "gh" => gh(rest.iter(budget), label),
            "curl" => {
                let words: Vec<&str> = rest.iter(budget).map(String::as_str).collect();
                curl::read(&words, &counts) != Curl::Fine
            }
            _ => false,
        };
        Ok(())
    });
    if found {
        vec![reason(label)]
    } else {
        Vec::new()
    }
}
