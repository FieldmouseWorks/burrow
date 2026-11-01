//! Saving a sandbox's work into the real project: every branch (as
//! refs/agents/ID/heads/*), plus everything in its working tree (uncommitted
//! and untracked, minus ignored) as one commit on top of its HEAD, at
//! refs/agents/ID/worktree.
//!
//! The VM could have written anything into its repo's config (hooks,
//! filters, fsmonitor), so nothing here runs git with that config where it
//! could execute code: the snapshot is made in a scratch repo that only
//! borrows the clone's objects. Nor with the host user's own or the system's
//! git config: the sandbox controls .gitattributes, which could name a
//! filter defined there.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::sandbox::Sandbox;
use crate::{Agent, hostname, stamp};

/// What saving found.
#[derive(Debug, PartialEq, Eq)]
pub enum Saved {
    /// Its branches and working tree are under refs/agents/ID/.
    Done,
    /// The project isn't a repository of its own (a worktree's .git is a
    /// file pointing into the real repo on the host): nothing to save to.
    NotARepository,
}

pub fn save(agent: &Agent, sb: &Sandbox) -> Result<Saved, String> {
    let cfg = agent.cfg;
    let repo = cfg.src.join(sb.project()?);
    let clone = sb.clone_path(cfg)?;
    let clone_git = clone.join(".git");
    if !clone_git.is_dir() || !repo.join(".git").is_dir() {
        return Ok(Saved::NotARepository);
    }
    let scratch = tempfile::Builder::new()
        .prefix("save.")
        .tempdir_in(&sb.dir)
        .map_err(|e| format!("a scratch repository in {}: {e}", sb.dir.display()))?;
    let t = scratch.path();
    let index = t.join("index");
    // Nor with anything git takes from the environment: each git here is
    // told all it needs.
    let inherited: Vec<_> = std::env::vars_os()
        .map(|(k, _)| k)
        .filter(|k| k.as_encoded_bytes().starts_with(b"GIT_"))
        .collect();
    let plain = || {
        let mut c = agent.command("git");
        for k in &inherited {
            c.env_remove(k);
        }
        c.env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(Stdio::null());
        c
    };
    let git = |dir: &Path| {
        let mut c = plain();
        c.arg(format!("--git-dir={}", dir.display()));
        c
    };
    out(plain().args(["init", "-q", "--bare"]).arg(t))?;
    let alternates = t.join("objects/info/alternates");
    fs::write(
        &alternates,
        format!("{}\n", clone_git.join("objects").display()),
    )
    .map_err(|e| format!("{}: {e}", alternates.display()))?;
    // The sandbox's own excludes, if it has any, so what it ignores stays out.
    let _ = fs::copy(clone_git.join("info/exclude"), t.join("info/exclude"));
    let refs = out(git(&clone_git).args([
        "for-each-ref",
        "--format=%(objectname) %(refname)",
        "refs/heads",
    ]))?;
    for line in refs.lines() {
        let Some((obj, name)) = line.split_once(' ') else {
            continue;
        };
        out(git(t).args(["update-ref", name, obj]))?;
    }
    // A repository with no commits yet still has its files: they become a
    // root commit.
    let head = git(&clone_git)
        .args(["rev-parse", "-q", "--verify", "HEAD^{commit}"])
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .filter(|h| !h.is_empty());
    out(git(t)
        .env("GIT_INDEX_FILE", &index)
        .arg(format!("--work-tree={}", clone.display()))
        .args(["add", "-A"]))?;
    let tree = out(git(t).env("GIT_INDEX_FILE", &index).arg("write-tree"))?;
    // By the agent, so it needs no identity from the host's git config.
    let mut commit = git(t);
    commit
        .args(["-c", &format!("user.name=agent {}", sb.id)])
        .args(["-c", &format!("user.email=agent@{}", hostname())])
        .args(["commit-tree", "--no-gpg-sign", tree.trim()]);
    if let Some(head) = &head {
        commit.args(["-p", head]);
    }
    commit.args([
        "-m",
        &format!("agent {}: working tree as of {}", sb.id, stamp()),
    ]);
    let obj = out(&mut commit)?;
    out(git(t).args(["update-ref", "refs/worktree", obj.trim()]))?;
    out(plain()
        .arg("-C")
        .arg(&repo)
        .args(["fetch", "-q", "--no-write-fetch-head"])
        .arg(t)
        .arg(format!("+refs/heads/*:refs/agents/{}/heads/*", sb.id))
        .arg(format!("+refs/worktree:refs/agents/{}/worktree", sb.id)))?;
    Ok(Saved::Done)
}

/// What a git command printed, or what it said when it failed.
fn out(c: &mut Command) -> Result<String, String> {
    let o = c.output().map_err(|e| format!("git: {e}"))?;
    if !o.status.success() {
        let args: Vec<String> = c
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        return Err(format!(
            "git {} failed ({}): {}",
            args.join(" "),
            o.status,
            String::from_utf8_lossy(&o.stderr).trim_end()
        ));
    }
    Ok(String::from_utf8_lossy(&o.stdout).into_owned())
}
