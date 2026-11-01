//! burrow-probe: read-only questions about the machine, asked on a private
//! repo's issues and answered there in seconds, with no Claude. An open issue
//! labelled `probe` (the config's `label`) in the config's repo, opened by
//! one of its authors, is a list of questions, one a line, in a fixed
//! vocabulary (query.rs); burrow-events runs a pass on the repo's issue
//! events, which answers each with one comment and closes it. Reopening it
//! asks again.
//!
//! What keeps it read-only and keeps secrets in: no shell, and nothing of the
//! issue's text reaches a command but a unit name or a time query.rs made;
//! journals only of the units on its allowlist; a repo that isn't private
//! gets no answer at all; and every answer goes through the scrubber
//! (scrub.rs) before it leaves. `burrow-probe CONFIG ask QUESTION` answers
//! one here, on the terminal, as it is: whoever runs it can read the same
//! things anyway.

pub mod answer;
pub mod pass;
pub mod query;
pub mod scrub;

use std::path::PathBuf;

use serde::Deserialize;

/// The probe's config, as modules/probe.nix writes it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// OWNER/NAME: the private repo whose issues it answers.
    pub repo: String,
    /// The label that makes an issue a probe.
    pub label: String,
    /// Who may ask (GitHub logins, any case).
    pub authors: Vec<String>,
    /// The machine's name, for the answer's first line.
    pub host: String,
    /// The user it runs as, whose user manager `units`, `rc` and `queue`
    /// read.
    pub user: String,
    /// The units whose journals it may read: globs (`*`, `?`, `[...]`).
    pub journal: Journal,
    /// Where HOST-update keeps live.json (burrow.stateDir).
    pub state_dir: PathBuf,
    /// proj, for `queue`; None where proj isn't on.
    pub proj: Option<PathBuf>,
    pub tools: Tools,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Journal {
    /// System units.
    pub units: Vec<String>,
    /// The user's own units (`journalctl --user-unit`).
    pub user_units: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Tools {
    pub journalctl: PathBuf,
    pub systemctl: PathBuf,
}

/// The mark on the probe's own comments.
pub const MARK: &str = "<!-- burrow-probe -->";
