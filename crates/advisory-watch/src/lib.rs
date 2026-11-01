//! advisory-watch: notice new vulnerabilities in what the machine and its
//! projects actually run, and get a Claude on each one the same hour. Its
//! jobs act on GitHub as the machine's own App, when it has one.
//!
//!   advisory-watch CONFIG          one pass (hourly; vulnix runs daily)
//!   advisory-watch CONFIG report   what it has seen lately
//!
//! modules/updates.nix builds the config in, as `advisory-watch`.
//!
//! Plain code does the filtering (sources.rs): only findings it hasn't seen
//! before, only for things that are really installed. What's left becomes a
//! Claude job, in the machine's own project or in the project, which decides
//! if it's real and fixes it, patching the machine ahead of the NixOS
//! channel when it has to (task.rs). Each source's first pass, and a new
//! project's, only records what's there, so a fresh install doesn't start
//! with a hundred jobs; Renovate's security PRs cover what was already open.
//! A finding is seen once its job has started: one that couldn't be handed
//! over waits for the next pass (state.rs).

pub mod config;
pub mod pass;
pub mod sources;
pub mod state;
pub mod task;

pub use config::Config;

use serde::{Deserialize, Serialize};

/// Something a source found: the key it's seen by, the line its job and the
/// log get, and the project whose job it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    pub project: String,
    pub key: String,
    pub text: String,
}

impl Finding {
    pub fn new(project: &str, key: &str, text: &str) -> Finding {
        Finding {
            project: project.to_owned(),
            key: key.to_owned(),
            text: text.to_owned(),
        }
    }
}

/// Now, as `date -Is` wrote it, in the machine's time zone.
pub fn stamp() -> String {
    jiff::Zoned::now()
        .strftime("%Y-%m-%dT%H:%M:%S%:z")
        .to_string()
}

/// Today, in the machine's time zone.
pub fn today() -> String {
    jiff::Zoned::now().strftime("%Y-%m-%d").to_string()
}
