//! What the watchdog keeps between passes, in its state directory.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::watch::Item;

/// state.json: the stalls it's taking a step at a time, the sources with no
/// watch file to read (since when), the stalls that have cleared whose
/// owner's notice is still to close, and its own jobs to withdraw once no
/// stall needs them, kept until proj has settled each.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct State {
    pub stalls: BTreeMap<String, Stall>,
    pub missing: BTreeMap<String, f64>,
    pub closing: BTreeMap<String, Stall>,
    pub withdraw: BTreeSet<String>,
}

/// One stall: the item as its source last had it, and how far it has gone.
/// A step's time is when it was taken.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct Stall {
    /// When it was first seen stalled.
    pub first: f64,
    pub source: String,
    /// The item's key in its source; "" for the source itself.
    pub key: String,
    #[serde(flatten)]
    pub item: Item,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kicked: Option<f64>,
    /// When a job couldn't start for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tried: Option<f64>,
    /// The job it was handed to, and when.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handed: Option<f64>,
    /// When it was first seen with its job over.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended: Option<f64>,
    /// More time a job gave it, until when, and why.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snoozed: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalated: Option<f64>,
    /// Its line on the stuck issue: why, what, and since when.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<[String; 3]>,
    /// When the owner's notice was last sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub told: Option<f64>,
}

/// A file of the state directory; `T::default()` when it's missing or isn't
/// one.
pub fn load<T: DeserializeOwned + Default>(dir: &Path, name: &str) -> T {
    fs::read(dir.join(name))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Write a file of the state directory whole: a reader sees the old one or
/// the new one.
pub fn save<T: Serialize>(dir: &Path, name: &str, value: &T) -> Result<(), String> {
    let path = dir.join(name);
    let tmp = dir.join(format!("{name}.tmp"));
    let text = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
    fs::rename(&tmp, &path).map_err(|e| format!("{}: {e}", path.display()))
}

/// What the stuck issue carries: every stall escalated to it, the ones the
/// owner has heard of since too (the issue's job keeps at it).
pub fn published(stalls: &BTreeMap<String, Stall>) -> Vec<[String; 3]> {
    let mut out: Vec<_> = stalls.values().filter_map(|s| s.entry.clone()).collect();
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::{State, load, save};

    // A state the Python watchdog wrote reads the same: its stalls carry on
    // from where they'd got to.
    #[test]
    fn reads_what_python_wrote() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("state.json"),
            r#"{"stalls": {"autoland\tsam/r#7\t1799989200": {"first": 1800000000.0, "source": "autoland",
                "key": "sam/r#7", "since": 1799989200, "due": 1799999940.0, "what": "PR #7", "phase": "CI",
                "link": null, "kick": "autoland/sam/r", "kicked": 1800000000.0, "job": "job-1",
                "handed": 1800000300.0, "escalated": 1800007500.0, "entry": ["a", "b", "c"]}},
              "missing": {"proj": 1799990000}, "closing": {}}"#,
        )
        .unwrap();
        let st: State = load(dir.path(), "state.json");
        let s = &st.stalls["autoland\tsam/r#7\t1799989200"];
        assert_eq!(
            (
                s.job.as_deref(),
                s.escalated,
                s.told,
                s.item.kick.as_deref()
            ),
            (
                Some("job-1"),
                Some(1800007500.0),
                None,
                Some("autoland/sam/r")
            )
        );
        assert_eq!(st.missing["proj"], 1799990000.0);
        save(dir.path(), "state.json", &st).unwrap();
        let again: State = load(dir.path(), "state.json");
        assert_eq!(again.stalls, st.stalls);
        assert!(!dir.path().join("state.json.tmp").exists());
        // Half a file is none.
        std::fs::write(dir.path().join("state.json"), "{half").unwrap();
        assert!(load::<State>(dir.path(), "state.json").stalls.is_empty());
    }
}
