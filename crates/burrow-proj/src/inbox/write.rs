//! What a pass changes on an issue besides its comments ([`Pass::post`]):
//! its labels, who it's assigned to, and closing it.

use super::Pass;
use serde_json::json;

impl Pass<'_> {
    pub(super) fn remove_label(&self, repo: &str, n: &str, label: &str) -> bool {
        crate::github::reported(burrow_github::call_allowing(
            &mut crate::github::client(self.cfg),
            "DELETE",
            &format!("repos/{repo}/issues/{n}/labels/{}", super::uri(label)),
            None,
            None,
            &[404],
        ))
        .is_some()
    }
    /// Close the issue with GitHub's completed or not_planned reason.
    pub(super) fn close(&self, repo: &str, n: &str, reason: &str) -> bool {
        crate::github::reported(burrow_github::call_once(
            &mut crate::github::client(self.cfg),
            "PATCH",
            &format!("repos/{repo}/issues/{n}"),
            Some(&json!({"state":"closed", "state_reason":reason})),
        ))
        .is_some()
    }
    pub(super) fn add_label(&self, repo: &str, n: &str, label: &str) -> bool {
        crate::github::reported(burrow_github::call_repeating(
            &mut crate::github::client(self.cfg),
            "POST",
            &format!("repos/{repo}/issues/{n}/labels"),
            Some(&json!({"labels":[label]})),
        ))
        .is_some()
    }
    pub(super) fn assign(&self, repo: &str, n: &str, login: &str) -> bool {
        crate::github::reported(burrow_github::call_repeating(
            &mut crate::github::client(self.cfg),
            "POST",
            &format!("repos/{repo}/issues/{n}/assignees"),
            Some(&json!({"assignees":[login]})),
        ))
        .is_some()
    }
}
