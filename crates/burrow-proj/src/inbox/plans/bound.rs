//! What a plan is bound to: the issue's text as it was planned from (its
//! title, its body and its authors' comments then). A task list goes on
//! with it and pauses without ([`Pass::pause`]); there, an author's comment
//! written since that opens with [`NOTE`] is a note on the list as it goes,
//! never scope: it neither pauses the list nor has it replanned.

use std::fs;

use serde_json::Value;

use super::{List, Pass};
use crate::inbox::raw;
use crate::inbox::record::files;

/// How an author's comment on a planned issue opens, in any case, to be a
/// note rather than a change to what was planned.
pub(in crate::inbox) const NOTE: &str = "Note:";

/// Whether this comment is a note ([`NOTE`]).
fn note(comment: &Value) -> bool {
    comment["body"]
        .as_str()
        .unwrap_or_default()
        .trim_start()
        .get(..NOTE.len())
        .is_some_and(|at| at.eq_ignore_ascii_case(NOTE))
}

impl Pass<'_> {
    /// Whether issue N is still what its plan was made from: its text and
    /// its authors' comments then, as they were, with nothing since but
    /// notes. None when it can't be told.
    pub(in crate::inbox) fn same_source(&self, repo: &str, issue: &Value) -> Option<bool> {
        let n = raw(&issue["number"]);
        let had = match files::input(&self.plan_dir(repo, &n)) {
            Ok(had) => had,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(false),
            Err(_) => return None,
        };
        if !self.list_bound(repo, &n, &had)? {
            return Some(false);
        }
        let written = self.written(repo, issue)?;
        let comments = &written.comments;
        // Only a task list takes notes: an approval to start is of the
        // issue as it stands, which is what its job is handed.
        let notes = self.has_task_list(Some(repo), &n);
        // The comments the plan was made with come first: every one after
        // them that's heard must be a note, so look back from the newest
        // for as long as that holds.
        for k in (0..=comments.len()).rev() {
            if self.issue_text(repo, &written.issue, &comments[..k]) == had {
                return Some(true);
            }
            let last = k.checked_sub(1).map(|i| &comments[i]);
            if !notes || last.is_none_or(|c| self.heard(c) && !note(c)) {
                break;
            }
        }
        Some(false)
    }

    pub(super) fn list_bound(&self, repo: &str, n: &str, input: &str) -> Option<bool> {
        match fs::read(self.plan_dir(repo, n).join("tasks.json")) {
            Ok(bytes) => serde_json::from_slice::<List>(&bytes)
                .ok()
                .map(|list| list.source.as_ref().is_none_or(|source| source == input)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(true),
            Err(_) => None,
        }
    }
}

#[cfg(test)]
#[path = "bound_tests.rs"]
mod tests;
