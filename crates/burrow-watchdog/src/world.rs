//! What a pass acts through: proj, burrow-events' kick port and notify.

use std::process::{Command, Stdio};
use std::time::Duration;

use crate::config::Config;

pub trait World {
    /// proj with these arguments: whether it succeeded, and what it printed.
    fn proj(&self, args: &[&str]) -> (bool, String);
    /// Start a burrow-events pass now (HANDLER[/OWNER/REPO]). Best effort: a
    /// kick that doesn't get through leaves a stall that goes on to its job.
    fn kick(&self, path: &str);
    /// notify with these arguments: its exit status, None if it didn't run.
    fn notify(&self, args: &[&str]) -> Option<i32>;
}

/// The machine's own.
pub struct Real<'a>(pub &'a Config);

impl World for Real<'_> {
    fn proj(&self, args: &[&str]) -> (bool, String) {
        match Command::new(&self.0.proj)
            .args(args)
            .stderr(Stdio::inherit())
            .output()
        {
            Ok(out) => (
                out.status.success(),
                String::from_utf8_lossy(&out.stdout).trim().to_owned(),
            ),
            Err(e) => {
                println!("couldn't run {}: {e}", self.0.proj.display());
                (false, String::new())
            }
        }
    }

    fn kick(&self, path: &str) {
        let url = format!("{}/{path}", self.0.kick);
        if let Err(e) = burrow_events::kick::kick(&url, Duration::from_secs(5)) {
            println!("couldn't kick {path}: {e}");
        }
    }

    fn notify(&self, args: &[&str]) -> Option<i32> {
        match Command::new(&self.0.notify).args(args).status() {
            Ok(s) => s.code(),
            Err(e) => {
                println!("couldn't run {}: {e}", self.0.notify);
                None
            }
        }
    }
}
