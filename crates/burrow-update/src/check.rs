//! A VM test, in one of the machine's heavy slots when it has them. A VM
//! test beside the machine's other gates and VM tests ran ten times its
//! usual length and past the test driver's hour; waiting for a slot happens
//! before the build starts, so it doesn't use up that hour, nor the run's
//! own limit (limit.rs). It waits ahead of other work (heavy's `--first`):
//! it's how a fix reaches the machine, and a run that went to the back of
//! the line for each test would see main move before its last. Once it has
//! the slot, it looks at main again before it starts (follow.rs's
//! `unless-moved`): a slot can take longer to come than main takes to move.

use std::fs::File;
use std::path::Path;
use std::process::Stdio;

use crate::Config;
use crate::follow::UNLESS_MOVED;
use crate::limit::Limit;
use crate::running::daemon_lost;

/// How a VM test went: passed, failed, or lost `nix-daemon` mid-run (which
/// says nothing about the tree or the test's reliability).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Check {
    Pass,
    Fail,
    DaemonLost,
}

/// How the VM test `name` goes on `commit`, with all it says in `log`:
/// in a slot, not when origin's main has moved past `main` by the time it
/// has one, bounded to `--max-jobs 1` so an aggregate check (`linkFarm` of
/// VM tests) never boots multiple VMs at once, and with the wait for its
/// slot handed back to `limit`.
pub(crate) fn run_check(
    cfg: &Config,
    limit: &Limit,
    name: &str,
    commit: &str,
    main: &str,
    log: &Path,
) -> Check {
    let Ok(out) = File::create(log) else {
        return Check::Fail;
    };
    let Ok(err) = out.try_clone() else {
        return Check::Fail;
    };
    let mut c = match &cfg.heavy {
        Some(heavy) => {
            let mut c = cfg.run_as_user(heavy);
            c.args(["--first", "--"])
                .arg(&cfg.program)
                .arg(UNLESS_MOVED)
                .arg(&cfg.work)
                .args([main, "--", "nix"]);
            // unless-moved's word alone; the test's all goes to stderr.
            c.stdout(Stdio::piped());
            c
        }
        None => {
            let mut c = cfg.run_as_user("nix");
            c.stdout(out);
            c
        }
    };
    let Ok(mut child) = c
        .args(["build", "--no-link", "-L", "--max-jobs", "1"])
        .arg(format!(
            "git+file://{}?rev={commit}#checks.{}.{name}",
            cfg.work.display(),
            cfg.system
        ))
        .stdin(Stdio::null())
        .stderr(err)
        .spawn()
    else {
        return Check::Fail;
    };
    if let Some(said) = child.stdout.take() {
        limit.wait(said);
    }
    if child.wait().is_ok_and(|s| s.success()) {
        Check::Pass
    } else if daemon_lost(log) {
        Check::DaemonLost
    } else {
        Check::Fail
    }
}

#[cfg(test)]
#[path = "check_tests.rs"]
mod tests;
