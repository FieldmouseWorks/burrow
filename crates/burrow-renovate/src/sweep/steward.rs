//! What the autoland steward last wrote about the repos it covers (its
//! repos.json): which it covers, written when, and the proofs it runs on a
//! machine rather than in CI, each with how. Read as the script's jq read
//! it: its repos matched without their case, and their proofs in the order
//! the file has them.

use std::fmt;
use std::fs;
use std::path::Path;

use serde::Deserialize;
use serde::de::{Deserializer, MapAccess, Visitor};
use serde_json::Value;

/// How long the steward's list is believed: a steward that stopped covers
/// nothing.
const BELIEVED: f64 = 6.0 * 3600.0;

/// A JSON value whose objects keep their entries in the file's order, as
/// jq keeps them.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum Json {
    Object(Entries),
    Other(Value),
}

/// An object's entries, in order.
#[derive(Debug)]
pub(crate) struct Entries(Vec<(String, Json)>);

impl<'de> Deserialize<'de> for Entries {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Entries, D::Error> {
        struct InOrder;
        impl<'de> Visitor<'de> for InOrder {
            type Value = Entries;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("an object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Entries, A::Error> {
                let mut entries = Vec::new();
                while let Some(entry) = map.next_entry()? {
                    entries.push(entry);
                }
                Ok(Entries(entries))
            }
        }
        d.deserialize_map(InOrder)
    }
}

impl Json {
    /// Its entry `key` (the last, as jq keeps one), if it's an object.
    fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(Entries(e)) => e.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v),
            Json::Other(_) => None,
        }
    }

    /// As a plain value, the way jq's string interpolation shows it.
    fn value(&self) -> Value {
        match self {
            Json::Object(Entries(e)) => {
                Value::Object(e.iter().map(|(k, v)| (k.clone(), v.value())).collect())
            }
            Json::Other(v) => v.clone(),
        }
    }
}

/// The steward's repos.json, if there's one it can read.
#[derive(Debug, Default)]
pub(crate) struct Steward(Option<Json>);

/// What the steward says of one repo.
#[derive(Debug, Default)]
pub(crate) struct Repo {
    /// Whether it covers it: it merges what's settled there.
    pub covered: bool,
    /// The names of the proofs it runs on a machine there, as values to
    /// match a check's name against, in jq's `keys` order.
    pub proof_names: Vec<Value>,
    /// How each proof runs, for the job: " NAME isn't CI: …" each, in the
    /// file's order.
    pub proofs: String,
}

impl Steward {
    /// The file at `path`; none at all when there's no steward (no path),
    /// or it isn't there or isn't JSON.
    pub fn read(path: Option<&Path>) -> Steward {
        let json = path
            .and_then(|p| fs::read(p).ok())
            .and_then(|text| serde_json::from_slice(&text).ok());
        Steward(json)
    }

    /// What it says of `repo`, its list believed if it's under six hours
    /// old at `now`. A list that says nothing of it, or nothing jq could
    /// read, covers it not and names no proofs there.
    pub fn repo(&self, repo: &str, now: i64) -> Repo {
        let Some(file) = &self.0 else {
            return Repo::default();
        };
        let fresh = match file.get("at") {
            Some(Json::Other(Value::Number(at))) => {
                at.as_f64().is_some_and(|at| (now as f64) - at < BELIEVED)
            }
            _ => false,
        };
        // jq's `.repos | with_entries(.key |= ascii_downcase) | .[$r | ascii_downcase]`.
        let want = repo.to_ascii_lowercase();
        let entry = match file.get("repos") {
            Some(Json::Object(Entries(e))) => e
                .iter()
                .rev()
                .find(|(k, _)| k.to_ascii_lowercase() == want)
                .map(|(_, v)| v),
            _ => return Repo::default(),
        };
        let Some(entry) = entry else {
            return Repo::default();
        };
        let Json::Object(_) = entry else {
            return Repo::default();
        };
        let covered = fresh && matches!(entry.get("covered"), Some(Json::Other(Value::Bool(true))));
        let (proof_names, proofs) = match entry.get("proofs") {
            Some(Json::Object(Entries(e))) => {
                let mut names: Vec<&String> = e.iter().map(|(k, _)| k).collect();
                names.sort();
                names.dedup();
                let how = e
                    .iter()
                    .map(|(name, how)| {
                        format!(
                            " {name} isn't CI: it runs on a machine, like this: {} Rerun it that way on what you push.",
                            super::shown(Some(&how.value()))
                        )
                    })
                    .collect();
                (
                    names
                        .into_iter()
                        .map(|n| Value::String(n.clone()))
                        .collect(),
                    how,
                )
            }
            _ => (Vec::new(), String::new()),
        };
        Repo {
            covered,
            proof_names,
            proofs,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn steward(text: &str) -> Steward {
        Steward(serde_json::from_str(text).ok())
    }

    #[test]
    fn a_repo_is_matched_without_its_case_and_its_proofs_kept_in_order() {
        let s = steward(
            r#"{"at": 1000, "repos": {"Sam/R": {"covered": true, "proofs": {"z-proof": "ssh z", "a-proof": {"run": 1}}}}}"#,
        );
        let r = s.repo("sam/r", 1000 + 3600);
        assert!(r.covered);
        assert_eq!(
            r.proof_names,
            [Value::from("a-proof"), Value::from("z-proof")]
        );
        assert_eq!(
            r.proofs,
            " z-proof isn't CI: it runs on a machine, like this: ssh z Rerun it that way on what you push. \
             a-proof isn't CI: it runs on a machine, like this: {\"run\":1} Rerun it that way on what you push."
        );
        // Six hours on, it covers nothing: the steward may have stopped. Its
        // proofs still run as they did.
        let stale = s.repo("sam/r", 1000 + 6 * 3600);
        assert!(!stale.covered);
        assert_eq!(stale.proof_names.len(), 2);
    }

    #[test]
    fn what_it_cant_read_covers_nothing_and_names_no_proofs() {
        for text in [
            "not json",
            r#"{"at": 1000}"#,
            r#"{"at": "1000", "repos": {"sam/r": {"covered": true}}}"#,
            r#"{"at": 1000, "repos": {"sam/r": {"covered": "yes"}}}"#,
            r#"{"at": 1000, "repos": {"sam/r": true}}"#,
            r#"{"at": 1000, "repos": {"sam/q": {"covered": true}}}"#,
            r#"{"at": 1000, "repos": {"sam/r": {"covered": true, "proofs": null}}}"#,
        ] {
            let r = steward(text).repo("sam/r", 1000);
            assert!(r.proof_names.is_empty() && r.proofs.is_empty(), "{text}");
            if !text.ends_with("null}}}") {
                assert!(!r.covered, "{text}");
            }
        }
        // Two spellings of one repo: the last is the one.
        let s = steward(
            r#"{"at": 1000, "repos": {"sam/r": {"covered": true}, "SAM/R": {"covered": false}}}"#,
        );
        assert!(!s.repo("sam/r", 1000).covered);
        assert!(!Steward::read(None).repo("sam/r", 0).covered);
    }
}
