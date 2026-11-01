//! HOST-update's steps in Rust. modules/updates/update.sh is the rest of
//! it: it runs as root and calls these (main.rs's words) where it once ran
//! bash of its own. They run as root too, with git and gh run as the
//! machine's user through runuser, the way update.sh's `as_user` runs them,
//! and what they keep between runs in HOST-update's state dir, which is
//! root's.
//!
//! train.rs is HOST-update's own PR, in a repo whose PRs land through its
//! merge train.

use std::ffi::OsString;
use std::process::{Command, Stdio};

use burrow_github::Gh;

pub mod config;
pub mod train;

pub use config::{Config, Train};

impl Config {
    /// What runs a program as the user, ahead of the program: runuser, with
    /// the user's home and runtime dir, as update.sh's `as_user` has it.
    pub fn as_user(&self) -> Vec<OsString> {
        vec![
            self.runuser.clone().into(),
            "-u".into(),
            self.user.clone().into(),
            "--".into(),
            "env".into(),
            format!("HOME={}", self.home).into(),
            format!("XDG_RUNTIME_DIR=/run/user/{}", self.uid).into(),
        ]
    }

    /// gh, as the user: the machine's gh, which takes the token of the
    /// account a call is about.
    pub fn gh(&self) -> Gh {
        Gh {
            token: None,
            via: self.as_user(),
        }
    }

    /// git in HOST-update's clone (`work`), as the user: what it printed,
    /// less its trailing newlines, or why it didn't. What it says on stderr
    /// goes to HOST-update's own, the journal.
    pub fn git(&self, args: &[&str]) -> Result<String, String> {
        self.git_with(args, Stdio::inherit())
    }

    /// git, with nothing said on stderr: for a failure the caller says
    /// in words of its own, as a refused push or a merge that conflicts.
    pub fn git_hushed(&self, args: &[&str]) -> Result<String, String> {
        self.git_with(args, Stdio::null())
    }

    fn git_with(&self, args: &[&str], stderr: Stdio) -> Result<String, String> {
        let [runuser, rest @ ..] = &self.as_user()[..] else {
            unreachable!("as_user starts with runuser")
        };
        let out = Command::new(runuser)
            .args(rest)
            .arg("git")
            .arg("-C")
            .arg(&self.work)
            .args(args)
            .stdin(Stdio::null())
            .stderr(stderr)
            .output()
            .map_err(|e| format!("{}: {e}", self.runuser.display()))?;
        if !out.status.success() {
            return Err(format!("git {}: {}", args.join(" "), out.status));
        }
        Ok(String::from_utf8_lossy(&out.stdout)
            .trim_end_matches('\n')
            .to_string())
    }
}
