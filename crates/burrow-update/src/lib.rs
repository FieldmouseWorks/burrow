//! HOST-update's steps in Rust. modules/updates/update.sh is the rest of
//! it: it runs as root and calls these (main.rs's words) where it once ran
//! bash of its own. They run as root too, with what they run (git, gh, nix,
//! proj) run as the machine's user through runuser, the way update.sh's
//! `as_user` runs them,
//! and what they keep between runs in HOST-update's state dir, which is
//! root's.
//!
//! - running.rs: what the machine runs, against what HEAD builds, and what
//!   a run found live
//! - push.rs: what went live, onto main
//! - check.rs: a VM test, in a heavy slot where the machine has them
//! - dispatch.rs: a problem, handed to a Claude job
//! - train.rs: HOST-update's own PR, in a repo whose PRs land through its
//!   merge train

use std::ffi::{OsStr, OsString};
use std::process::{Command, Stdio};

use burrow_github::Gh;

pub mod check;
pub mod config;
pub mod dispatch;
pub mod push;
pub mod running;
pub mod train;

#[cfg(test)]
mod testing;

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
        let out = self
            .run_as_user("git")
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

    /// `program`, to run as the user (`as_user`), its arguments still to
    /// come.
    pub fn run_as_user(&self, program: impl AsRef<OsStr>) -> Command {
        let [runuser, rest @ ..] = &self.as_user()[..] else {
            unreachable!("as_user starts with runuser")
        };
        let mut c = Command::new(runuser);
        c.args(rest).arg(program);
        c
    }

    /// What HOST-update recorded in its state file `name`, less its trailing
    /// newlines: nothing when there's no such file, as `cat` reads it.
    pub fn recorded(&self, name: &str) -> String {
        std::fs::read_to_string(self.state.join(name))
            .unwrap_or_default()
            .trim_end_matches('\n')
            .to_string()
    }
}

/// Whether `s` is a full commit id, as git prints one: 40 lowercase hex
/// digits, or 64 in a SHA-256 repo.
pub fn is_commit(s: &str) -> bool {
    [40, 64].contains(&s.len()) && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Whether `s` is a name update.sh gives a step: a VM test's (an attribute
/// of the flake's checks) or a problem's (a file in jobs/). Letters, digits,
/// - and _, and not -first, so it's neither a path nor an option.
pub fn is_name(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('-')
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

/// The last line a program printed when it went, as `| tail -n1` takes it,
/// or nothing when it failed or printed none.
pub fn last_line(out: std::io::Result<std::process::Output>) -> Option<String> {
    let out = out.ok().filter(|o| o.status.success())?;
    let text = String::from_utf8_lossy(&out.stdout);
    Some(text.lines().last()?.to_string()).filter(|l| !l.is_empty())
}
