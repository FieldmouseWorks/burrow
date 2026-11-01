//! A VM test, in one of the machine's heavy slots when it has them. A VM
//! test beside the machine's other gates and VM tests ran ten times its
//! usual length and past the test driver's hour; waiting for a slot happens
//! before the build starts, so it doesn't use up that hour.

use std::fs::File;
use std::path::Path;
use std::process::Stdio;

use crate::Config;

/// Whether the VM test `name` passes on `commit`, with all it says in `log`.
pub fn run_check(cfg: &Config, name: &str, commit: &str, log: &Path) -> bool {
    let Ok(out) = File::create(log) else {
        return false;
    };
    let Ok(err) = out.try_clone() else {
        return false;
    };
    let mut c = match &cfg.heavy {
        Some(heavy) => {
            let mut c = cfg.run_as_user(heavy);
            c.args(["--", "nix"]);
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
