//! A VM test, in one of the machine's heavy slots when it has them. A VM
//! test beside the machine's other gates and VM tests ran ten times its
//! usual length and past the test driver's hour; waiting for a slot happens
//! before the build starts, so it doesn't use up that hour. Once it has
//! the slot, it looks at main again before it starts (follow.rs's
//! `unless-moved`): a slot can take longer to come than main takes to move.

use std::fs::File;
use std::path::Path;
use std::process::Stdio;

use crate::Config;
use crate::follow::UNLESS_MOVED;

/// Whether the VM test `name` passes on `commit`, with all it says in `log`:
/// in a slot, not when origin's main has moved past `main` by the time it
/// has one.
pub fn run_check(cfg: &Config, name: &str, commit: &str, main: &str, log: &Path) -> bool {
    let Ok(out) = File::create(log) else {
        return false;
    };
    let Ok(err) = out.try_clone() else {
        return false;
    };
    let mut c = match &cfg.heavy {
        Some(heavy) => {
            let mut c = cfg.run_as_user(heavy);
            c.arg("--")
                .arg(&cfg.program)
                .arg(UNLESS_MOVED)
                .arg(&cfg.work)
                .args([main, "--", "nix"]);
            c
        }
        None => cfg.run_as_user("nix"),
    };
    c.args(["build", "--no-link", "-L"])
        .arg(format!(
            "git+file://{}?rev={commit}#checks.{}.{name}",
            cfg.work.display(),
            cfg.system
        ))
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(test)]
#[path = "check_tests.rs"]
mod tests;
