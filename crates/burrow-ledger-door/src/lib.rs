//! burrow-ledger-door: undo, asked for on a repo's issues and opened with no
//! Claude in between, as the probe answers facts. An open issue labelled
//! `undo` (the config's `label`) in the config's repo, written only by its
//! authors (burrow-door), asks for one undo, in the words `cn undo` takes:
//!
//! - `undo OWNER/REPO#N`: the change that PR is;
//! - `undo --actor SESSION`: everything an agent session landed;
//! - `undo --since TIME --repo OWNER/REPO`: everything that landed on the
//!   repo after TIME (RFC 3339).
//!
//! burrow-events runs a pass on the repo's issue events, which runs
//! `cn undo` on the ledger, as its writers do, and answers with the undo's
//! PR (or why there's none), then closes the issue. The PR is a change like
//! any other: the steward settles it, and an undo is undone in turn the
//! same way, so this door opens nothing that can't be shut.
//!
//! Nothing of the issue's text reaches a shell: a request is parsed into
//! cn's arguments, each one checked for its shape. What it may reach is
//! the ledger's to say, as cn refuses what the ledger keeps nothing of.

pub mod pass;
pub mod request;

use std::path::PathBuf;

use serde::Deserialize;

/// The door's config, as modules/ledger.nix writes it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// OWNER/NAME: the repo whose issues it answers.
    pub repo: String,
    /// The label that makes an issue a request.
    pub label: String,
    /// Who may ask (GitHub logins, any case).
    pub authors: Vec<burrow_github::Author>,
    /// The machine's name, for the answer's first line.
    pub host: String,
    /// `cn`, on the ledger, as its writers run it.
    pub cn: PathBuf,
}

/// The mark on the door's own comments.
pub const MARK: &str = "<!-- burrow-ledger-door -->";
