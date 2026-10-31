//! What the dispatcher keeps between passes, in its state directory.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// state.json.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct State {
    /// Each runner a pass found idle with jobs queued for it, since when.
    pub idle: BTreeMap<String, f64>,
    /// When a pass restarted (or started) each runner, the latest few.
    pub restarts: BTreeMap<String, Vec<f64>>,
    /// Each repo GitHub couldn't be read for, since when.
    pub unread: BTreeMap<String, f64>,
    /// Each repo whose main isn't live in the system that runs, or isn't
    /// known to be, since when: its oldest commit not live, or the pass that
    /// first couldn't tell. A run of HOST-update, under way or failed,
    /// doesn't move it.
    pub going_live: BTreeMap<String, f64>,
    /// When a pass tried to cancel each run (OWNER/REPO#ID), whatever came
    /// of it: one is tried once in RECENT.
    pub cancels: BTreeMap<String, f64>,
    /// What passes did, newest last: the latest LOG of them.
    pub log: Vec<Done>,
}

/// One move a pass made.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct Done {
    pub at: f64,
    pub what: String,
    pub why: String,
    /// What came of it: "ok", or why not.
    pub result: String,
}
