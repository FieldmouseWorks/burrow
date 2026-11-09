//! Require a routing role for unattended unit starts, while preserving the
//! ordinary CLI used inside Remote Control and existing agent jobs.

use std::{env, io, path::Path};

use crate::{Fail, io_fail};

pub(super) fn check(
    role: Option<&str>,
    cgroup: impl FnOnce() -> io::Result<String>,
) -> Result<(), Fail> {
    if role.is_some()
        || env::var_os("INVOCATION_ID").is_none()
        || env::var_os("PROJ_JOB").is_some_and(|id| !id.is_empty())
    {
        return Ok(());
    }
    // INVOCATION_ID is inherited by every child of a service. The kernel's
    // unit identity distinguishes existing Remote Control sessions without
    // requiring their long-running servers to restart for a new env marker.
    let groups = cgroup().map_err(|e| io_fail(Path::new("/proc/self/cgroup"), e))?;
    if remote_control(&groups) {
        return Ok(());
    }
    Err(Fail::new(
        2,
        "proj run from a machine unit needs --role NAME",
    ))
}

fn remote_control(groups: &str) -> bool {
    groups
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(3, ':');
            let hierarchy = fields.next()?;
            let controllers = fields.next()?;
            let path = fields.next()?;
            ((hierarchy == "0" && controllers.is_empty())
                || controllers.split(',').any(|c| c == "name=systemd"))
            .then_some(path)
        })
        .flat_map(|path| path.split('/'))
        .any(|unit| unit.starts_with("claude-rc@") && unit.ends_with(".service"))
}

#[test]
fn only_remote_control_unit_identity_exempts_a_session() {
    for path in [
        "0::/user.slice/user@1001.service/claude.slice/claude-rc.slice/claude-rc@project.service",
        "1:name=systemd:/user.slice/claude-rc@agent\\x20pipeline.service",
        "0::/user.slice/claude-rc@project.service/child",
    ] {
        assert!(remote_control(path), "{path}");
    }
    for path in [
        "0::/system.slice/advisory-watch.service",
        "0::/user.slice/claude-rc.slice/claude-rc-scan.service",
        "0::/user.slice/proj-inbox.service",
        "0::/user.slice/not-claude-rc@project.service",
        "0::/user.slice/claude-rc@project.scope",
        "1:memory:/user.slice/claude-rc@project.service",
        "",
    ] {
        assert!(!remote_control(path), "{path}");
    }
}
