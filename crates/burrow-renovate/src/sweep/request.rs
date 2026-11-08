//! Asking Renovate to redo a branch, and taking the request back: its
//! rebase label, and the rebase! title prefix the sweep only ever takes off.

use serde_json::json;

use serde_json::Value;

use super::Sweep;

/// The label that has Renovate redo the branch on its next run, which takes
/// it off once it has (its rebaseLabel, modules/updates/renovate-policy.nix;
/// modules/autoland/lock_wait.py's REQUEST). One run: a "rebase!" title has
/// it redo the branch on every run until someone takes it off, and each of
/// those pushes starts the next run (TusanHomichi/career#22, 6,492 CI runs).
pub(super) const REQUEST: &str = "rebase";
/// The title prefix that has Renovate redo the branch on every run, which
/// anyone can still type (lock_wait.py's TITLE): it comes off on sight.
pub(super) const TITLE: &str = "rebase!";

impl Sweep<'_> {
    /// When a refresh request on PR `n` last came off, as its timeline has
    /// it: the rebase label taken off, or a rebase! title changed. Some(None)
    /// when one never did, None when the timeline can't be read: gh failing
    /// on any page counts, though what it printed may still parse (an empty
    /// or a partial list).
    pub(super) fn request_gone(&self, n: &str) -> Option<Option<i64>> {
        let pages = self
            .cfg
            .pages(&format!("repos/{}/issues/{n}/timeline", self.repo))?;
        Some(
            pages
                .iter()
                .filter_map(Value::as_array)
                .flatten()
                .filter(|e| match e["event"].as_str() {
                    Some("unlabeled") => e["label"]["name"] == REQUEST,
                    Some("renamed") => e["rename"]["from"]
                        .as_str()
                        .is_some_and(|t| t.starts_with(TITLE)),
                    _ => false,
                })
                .filter_map(|e| e["created_at"].as_str()?.parse::<jiff::Timestamp>().ok())
                .map(|at| at.as_second())
                .max(),
        )
    }

    /// PR `n` retitled `title`, whatever GitHub says to it.
    pub(super) fn retitle(&self, n: &str, title: &str) {
        let cfg = self.cfg;
        let path = format!("repos/{}/pulls/{n}", self.repo);
        let _ = burrow_github::call_once(
            &mut cfg.github(),
            "PATCH",
            &path,
            Some(&json!({"title":title})),
        );
    }

    /// PR `n` given Renovate's rebase label (`on`) or rid of it, whatever
    /// GitHub says to it.
    pub(super) fn request(&self, n: &str, on: bool) {
        let cfg = self.cfg;
        let repo = self.repo;
        let (method, path) = if on {
            ("POST", format!("repos/{repo}/issues/{n}/labels"))
        } else {
            (
                "DELETE",
                format!("repos/{repo}/issues/{n}/labels/{REQUEST}"),
            )
        };
        let body = on.then(|| json!({"labels":[REQUEST]}));
        let _ = burrow_github::call_repeating(&mut cfg.github(), method, &path, body.as_ref());
    }
}
