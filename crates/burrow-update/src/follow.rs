//! A run follows main. It takes origin's main when it begins, but the train
//! can land the PRs of one candidate seconds apart, each push starting a
//! run: the first one's main is a tree nobody gated, gone before its first
//! VM test. So before each VM test it asks for a slot for, inside that slot
//! just before the test starts, and before the switch, it looks at origin's
//! main again. When main has moved, it starts over from the new head
//! (run.rs's `once`), and the memo keeps what already passed; nothing that
//! ran on the old one is counted against it.
//!
//! Inside the slot it's `unless-moved`, a word of burrow-update's own that
//! heavy runs as the user: the run, as root, isn't in there. A look that
//! can't reach origin goes on: it's no sign that main moved. It says it's
//! in the slot first (`IN_SLOT`), which ends the run's wait (limit.rs).

use std::ffi::OsString;
use std::io::Write as _;
use std::os::fd::AsFd as _;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::run::{End, Run, Step};

/// The word heavy runs a VM test through: `burrow-update unless-moved WORK
/// MAIN -- COMMAND...`.
pub const UNLESS_MOVED: &str = "unless-moved";

/// All `unless-moved` says on stdout, as it starts: that it has its slot.
pub(crate) const IN_SLOT: &str = "in a slot";

impl Run<'_> {
    /// That origin's main is still the one this run took: it fetches it,
    /// and ends the run to start over (`End::Moved`) when it moved.
    pub(crate) fn still_main(&self) -> Step {
        if let Err(why) = self.cfg.git(&["fetch", "-q", "origin", "main"]) {
            println!("{why}; going on with main as it was");
            return Ok(());
        }
        let now = self.git(&["rev-parse", "refs/remotes/origin/main"])?;
        if now == self.main {
            Ok(())
        } else {
            Err(End::Moved(now))
        }
    }
}

/// `unless-moved`: says it's in its slot, then runs COMMAND, all it says
/// going to stderr (the test's log), unless origin's main has moved past
/// `main` (`moved`); then it says so and exits 1. Its exit, when COMMAND
/// can't start.
pub fn unless_moved(work: &Path, main: &str, command: &[OsString]) -> u8 {
    if let Err(e) = writeln!(std::io::stdout(), "{IN_SLOT}") {
        eprintln!("couldn't say it's in its slot: {e}");
    }
    if let Some(now) = moved(work, main) {
        eprintln!("main moved to {now}: not testing a tree on {main}");
        return 1;
    }
    let Some((program, args)) = command.split_first() else {
        return 2;
    };
    let log = match std::io::stderr().as_fd().try_clone_to_owned() {
        Ok(log) => log,
        Err(e) => {
            eprintln!("stderr: {e}");
            return 127;
        }
    };
    let e = Command::new(program).args(args).stdout(log).exec();
    eprintln!("{}: {e}", program.to_string_lossy());
    127
}

/// Where origin's main is now, fetched into the clone `work` by plain git
/// as whoever runs it, when that's past `main`: nothing when it's still
/// `main` or git couldn't say.
pub fn moved(work: &Path, main: &str) -> Option<String> {
    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(work)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
    };
    let Some(o) = git(&["fetch", "-q", "origin", "main"])
        .and_then(|_| git(&["rev-parse", "refs/remotes/origin/main"]))
    else {
        eprintln!("couldn't look at origin's main; going on");
        return None;
    };
    let now = String::from_utf8_lossy(&o.stdout).trim_end().to_string();
    (now != main).then_some(now)
}

#[cfg(test)]
#[path = "follow_tests.rs"]
mod tests;
