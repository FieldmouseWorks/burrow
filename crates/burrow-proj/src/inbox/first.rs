//! `first` on an issue: the steward's own label (burrow.autoland.labels.first,
//! [`crate::config::Inbox::first_label`]), which one of the authors puts on
//! work that should start at once and land ahead of the rest. The drain
//! hands it on ahead of every priority, its job takes the next free slot
//! ahead of jobs without it, and `later` doesn't park it. The steward reads
//! the same label off the issue a PR closes and puts it on the PR
//! (modules/autoland/firsts.py).
//!
//! It counts only when the last time the label went on, one of the authors
//! put it there, by account id, as GitHub's `labeled` event names its
//! actor: never the issue's text, so a job acting as the workers App can't
//! move its own work up the line. It changes order only.

use serde_json::Value;

use super::raw;
use crate::config::{Config, Inbox};
use crate::github::every;

/// In a ready list's entry ([`super::drain::READY_LISTS`]), and on an issue
/// the drain read afresh: whether its `first` counted when it was read.
pub(super) const MARKED: &str = "marked_first";

/// Whether the issue carries the label, whoever put it on.
pub(super) fn labelled(inbox: &Inbox, issue: &Value) -> bool {
    issue["labels"].as_array().is_some_and(|ls| {
        ls.iter()
            .any(|l| l["name"].as_str() == Some(inbox.first_label.as_str()))
    })
}

/// Whether an author's `first` is on the issue: it carries the label, and
/// the last event that put it there was one of the authors'. GitHub is asked
/// only about an issue that carries it. Err when GitHub won't say.
pub(super) fn marked(
    cfg: &Config,
    inbox: &Inbox,
    repo: &str,
    issue: &Value,
) -> Result<bool, String> {
    if !labelled(inbox, issue) {
        return Ok(false);
    }
    let n = raw(&issue["number"]);
    let events = every(cfg, &format!("repos/{repo}/issues/{n}/events?per_page=100"))
        .ok_or_else(|| format!("cannot read {repo}#{n}'s label events"))?;
    Ok(by_author(inbox, &events))
}

/// What [`marked`] or the pass that listed it found, on an issue read here.
pub(super) fn noted(issue: &Value) -> bool {
    issue[MARKED].as_bool() == Some(true)
}

/// Of an issue's events, oldest first: whether the last that put the label
/// on was by one of the authors.
fn by_author(inbox: &Inbox, events: &[Value]) -> bool {
    events
        .iter()
        .rev()
        .find(|e| {
            e["event"] == "labeled" && e["label"]["name"].as_str() == Some(&inbox.first_label)
        })
        .is_some_and(|e| {
            let id = e["actor"]["id"].as_u64();
            inbox.authors.iter().any(|a| a.matches(id))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn inbox() -> Inbox {
        let cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
        cfg.inbox.unwrap()
    }

    fn labeled(by: &str, label: &str) -> Value {
        json!({"event": "labeled", "label": {"name": label}, "actor": burrow_github::fake::actor(by)})
    }

    #[test]
    fn only_an_authors_last_labelling_counts() {
        let i = inbox();
        assert!(by_author(&i, &[labeled("sam", "first")]));
        // The workers App, a job acting as it, or anyone else: not theirs.
        assert!(!by_author(&i, &[labeled("workers[bot]", "first")]));
        assert!(!by_author(&i, &[labeled("mallory", "first")]));
        // A login spelled like an author's, with another account's id.
        let mut spoof = labeled("sam", "first");
        spoof["actor"]["id"] = json!(7);
        assert!(!by_author(&i, &[spoof]));
        // The last time it went on is what counts.
        assert!(!by_author(
            &i,
            &[labeled("sam", "first"), labeled("workers[bot]", "first")]
        ));
        assert!(by_author(
            &i,
            &[labeled("workers[bot]", "first"), labeled("sam", "first")]
        ));
        // Another label's event says nothing about this one.
        assert!(!by_author(&i, &[labeled("sam", "priority: high")]));
        assert!(!by_author(&i, &[]));
    }
}
