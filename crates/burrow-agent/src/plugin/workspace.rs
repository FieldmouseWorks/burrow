//! A job's workspace: a clone of its project, as `agent new` makes one, so
//! nothing the plugin writes is the host's project. It comes back as
//! `agent fetch` brings any sandbox's work back.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use super::render::Workspace;
use crate::sandbox::{self, Sandbox};
use crate::{Agent, as_account, confine, new, stderr, touch, write_line};

/// A file of the call's, naming the sandbox it made.
const MADE: &str = "sandbox";
/// A file of the call's: that sandbox's work was brought back.
const SAVED: &str = "saved";

/// The call's clone, ready to share.
pub(crate) fn make(
    agent: &Agent,
    call: &Path,
    engine: &str,
    workspace: &Workspace,
    git_metadata: bool,
) -> Result<(Sandbox, PathBuf), String> {
    let cfg = agent.cfg;
    let (_, id, dir) = new::clone(agent, &workspace.project)?;
    let at = |name: &str| dir.join(name);
    write_line(&at(sandbox::PROJECT), &workspace.project)?;
    write_line(&at(sandbox::PLUGIN), engine)?;
    write_line(&at(sandbox::TTL_DAYS), "7")?;
    touch(&at(sandbox::LAST_USED))?;
    write_line(&call.join(MADE), &id)?;
    let mut chown = vec![
        format!("{}:kvm", cfg.user),
        dir.to_string_lossy().into_owned(),
    ];
    for name in [
        sandbox::PROJECT,
        sandbox::PLUGIN,
        sandbox::TTL_DAYS,
        sandbox::LAST_USED,
    ] {
        chown.push(at(name).to_string_lossy().into_owned());
    }
    agent.run("chown", &chown)?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o770))
        .map_err(|e| format!("{}: {e}", dir.display()))?;

    let sandbox = Sandbox::find(cfg, &id)?;
    let clone = sandbox.clone_path(cfg)?;
    // A link anywhere on the way would share what it points to instead.
    let real = |p: &Path| {
        p.canonicalize()
            .map_err(|e| format!("{}: {e}", p.display()))
    };
    let expected = real(&dir)?
        .join("data")
        .join(cfg.rel()?)
        .join(&workspace.project);
    if real(&clone)? != expected {
        return Err(format!("{} isn't the project's own clone", clone.display()));
    }
    if git_metadata {
        confine::sanitize(agent, &clone.join(".git"))?;
    }
    Ok((sandbox, clone))
}

/// Brings the work back as the user. Says where it is, or why it's kept.
pub(crate) fn save(agent: &Agent, call: &Path, sandbox: &Sandbox) -> Result<String, String> {
    let cfg = agent.cfg;
    let status = as_account(agent, &cfg.user, &cfg.group)
        .arg(&agent.exe)
        .arg(&agent.config)
        .args(["fetch", &sandbox.id])
        .env("HOME", &cfg.home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr()?)
        .status()
        .map_err(|e| format!("agent fetch: {e}"))?;
    if !status.success() {
        return Err(format!(
            "its work couldn't be saved ({status}), so {} is kept: agent fetch {}",
            sandbox.id, sandbox.id
        ));
    }
    touch(&call.join(SAVED))?;
    Ok(format!("its work is at refs/agents/{}/", sandbox.id))
}

/// After a call, once nothing of it runs: saves its clone's work if the
/// call didn't get to, then deletes it. One it can't save stays, as a
/// sandbox `agent ls` shows.
pub fn end(agent: &Agent, call: &Path) -> Result<(), String> {
    let made = call.join(MADE);
    if !made.exists() {
        return Ok(());
    }
    let sandbox = Sandbox::find(agent.cfg, &crate::read(&made)?)?;
    if !call.join(SAVED).exists() {
        eprintln!("{}: {}", sandbox.id, save(agent, call, &sandbox)?);
    }
    sandbox.destroy(agent)
}
