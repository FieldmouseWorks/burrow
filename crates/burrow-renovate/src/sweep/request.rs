//! Asking Renovate to redo a branch, and taking the request back: its
//! rebase label, and the rebase! title prefix the sweep only ever takes off.

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
    /// when one never did, None when the timeline can't be read.
    pub(super) fn request_gone(&self, n: &str) -> Option<Option<i64>> {
        let cfg = self.cfg;
        let said = cfg.said(
            &cfg.tools.gh,
            &[
                "api",
                "--paginate",
                "--slurp",
                &format!("repos/{}/issues/{n}/timeline", self.repo),
            ],
        );
        let pages: Vec<Vec<Value>> = serde_json::from_slice(&said).ok()?;
        Some(
            pages
                .iter()
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
        let title = format!("title={title}");
        cfg.quietly(&cfg.tools.gh, &["api", "-X", "PATCH", &path, "-f", &title]);
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
        let label = format!("labels[]={REQUEST}");
        let mut args = vec!["api", "-X", method, &path];
        if on {
            args.extend(["-f", &label]);
        }
        cfg.quietly(&cfg.tools.gh, &args);
    }
}
