//! Git over a clone a sandbox wrote, confined to that clone and what the
//! step writes. A sandbox can put a link anywhere in its repository
//! (`objects`, `refs`, `commondir`, `info/exclude`), and git on the host
//! would follow it to whatever the user can read.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::sandbox::Sandbox;
use crate::{Agent, as_account, exit_code, stderr};

/// The seconds a confined step gets. A sandbox can leave git a fifo to
/// wait on.
const SECONDS: &str = "600";

/// This program again under `command`, a bwrap, seeing the store, `bind`'s
/// paths and nothing else of the host's.
fn confined(agent: &Agent, mut command: Command, bind: impl FnOnce(&mut Command)) -> Command {
    command
        .args(["--unshare-all", "--die-with-parent"])
        .args(["--ro-bind", "/nix/store", "/nix/store"])
        .args(["--ro-bind-try", "/bin", "/bin"])
        .args(["--ro-bind-try", "/etc/passwd", "/etc/passwd"])
        .args(["--ro-bind-try", "/etc/localtime", "/etc/localtime"])
        .args(["--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp"])
        .args(["--setenv", "TMPDIR", "/tmp"]);
    bind(&mut command);
    command
        .args(["--chdir", "/", "--"])
        .args(["timeout", "--kill-after=5", SECONDS])
        .arg(&agent.exe)
        .arg(&agent.config);
    command
}

fn plain_dir(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
}

/// `agent _save ID`, confined, as the user who called: what it exited with.
/// It sees the sandbox's directory, its clone of the project (read-only)
/// and the project's repository, not the rest of the data's clone.
pub fn save(agent: &Agent, sb: &Sandbox) -> Result<i32, String> {
    let cfg = agent.cfg;
    let (clone, repo) = (
        sb.clone_path(cfg)?,
        cfg.src.join(sb.project()?).join(".git"),
    );
    // bwrap follows a link in a path it binds, and a sandbox given a folder
    // above the project (`--with`) could have made the clone one.
    let real = |p: &Path| {
        p.canonicalize()
            .map_err(|e| format!("{}: {e}", p.display()))
    };
    let expected = real(&sb.dir)?
        .join("data")
        .join(cfg.rel()?)
        .join(sb.project()?);
    if real(&clone)? != expected {
        return Err(format!("{} isn't the project's own clone", clone.display()));
    }
    let mut command = confined(agent, agent.command("bwrap"), |c| {
        c.arg("--bind").arg(&sb.dir).arg(&sb.dir);
        c.arg("--tmpfs").arg(sb.dir.join("data"));
        c.arg("--ro-bind").arg(&clone).arg(&clone);
        if plain_dir(&repo) {
            c.arg("--bind").arg(&repo).arg(&repo);
            // It brings refs and objects, never what the host's git runs.
            for part in ["config", "hooks"] {
                let at = repo.join(part);
                c.arg("--ro-bind-try").arg(&at).arg(&at);
            }
        }
    });
    let status = command
        .args(["_save", &sb.id])
        .stdin(Stdio::null())
        .status()
        .map_err(|e| format!("bwrap: {e}"))?;
    Ok(exit_code(status))
}

/// `agent _sanitize GIT`, confined to that directory, as the user, from
/// root's service.
pub fn sanitize(agent: &Agent, git: &Path) -> Result<(), String> {
    if !plain_dir(git) {
        return Err(format!("{}: not a directory", git.display()));
    }
    let mut account = as_account(agent, &agent.cfg.user, &agent.cfg.group);
    account.arg("bwrap");
    let status = confined(agent, account, |c| {
        c.arg("--bind").arg(git).arg(git);
    })
    .arg("_sanitize")
    .arg(git)
    .stdin(Stdio::null())
    .stdout(stderr()?)
    .stderr(stderr()?)
    .status()
    .map_err(|e| format!("bwrap: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "{}'s config couldn't be cleaned ({status})",
            git.display()
        ))
    }
}
