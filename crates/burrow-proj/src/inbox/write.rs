//! What a pass changes on an issue besides its comments ([`Pass::post`]):
//! its labels, who it's assigned to, and closing it.

use std::process::{Command, Stdio};

use super::Pass;

impl Pass<'_> {
    /// `gh api ARGS`, its answer thrown away: whether GitHub took it.
    fn write(&self, args: &[&str]) -> bool {
        Command::new(&self.cfg.tools.gh)
            .arg("api")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    /// Close the issue, as completed or not planned (GitHub's state_reason).
    pub(super) fn close(&self, repo: &str, n: &str, reason: &str) -> bool {
        self.write(&[
            "-X",
            "PATCH",
            &format!("repos/{repo}/issues/{n}"),
            "-f",
            "state=closed",
            "-f",
            &format!("state_reason={reason}"),
        ])
    }

    /// Add a label to the issue; the ones it has stay.
    pub(super) fn add_label(&self, repo: &str, n: &str, label: &str) -> bool {
        self.write(&[
            "-X",
            "POST",
            &format!("repos/{repo}/issues/{n}/labels"),
            "-f",
            &format!("labels[]={label}"),
        ])
    }

    /// Assign the issue to a login, as well as whoever has it.
    pub(super) fn assign(&self, repo: &str, n: &str, login: &str) -> bool {
        self.write(&[
            "-X",
            "POST",
            &format!("repos/{repo}/issues/{n}/assignees"),
            "-f",
            &format!("assignees[]={login}"),
        ])
    }
}
