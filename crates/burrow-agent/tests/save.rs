//! `agent fetch`'s save, against real git: a sandbox's clone of a project,
//! with whatever the sandbox put in its repository's config, saved into the
//! real project on the host.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use burrow_agent::sandbox::Sandbox;
use burrow_agent::save::{Saved, save};
use burrow_agent::{Agent, Config};

/// A machine in a temporary directory: the projects folder on the data
/// dataset, and a sandbox `demo-1` whose clone of it holds `demo`.
struct Machine {
    _dir: tempfile::TempDir,
    at: PathBuf,
    cfg: Config,
}

impl Machine {
    fn new() -> Machine {
        let dir = tempfile::tempdir().unwrap();
        let at = dir.path().canonicalize().unwrap();
        let cfg: Config = serde_json::from_value(serde_json::json!({
            "runner": "/nix/store/x-runner",
            "forwardFiltered": "/nix/store/x-proxy-3128",
            "forwardOpen": "/nix/store/x-proxy-3129",
            "root": at.join("agents"),
            "src": at.join("data/dev/src"),
            "data": at.join("data"),
            "dataDataset": "rpool/safe/data",
            "dataset": "rpool/agents",
            "user": "dev",
            "uid": 1000,
            "group": "users",
            "home": at.join("home"),
            "tokenFile": null,
            "sudo": "/run/wrappers/bin/sudo",
            "path": "/nonexistent"
        }))
        .unwrap();
        let m = Machine { _dir: dir, at, cfg };
        let repo = m.repo();
        fs::create_dir_all(&repo).unwrap();
        m.git(&repo, &["init", "-q", "-b", "main"]);
        fs::write(repo.join("a"), "one\n").unwrap();
        m.git(&repo, &["add", "a"]);
        m.git(&repo, &["commit", "-qm", "one"]);
        // The sandbox's clone, as `agent new` leaves it: the data dataset
        // as of then.
        let sb = m.at.join("agents/demo-1");
        fs::create_dir_all(sb.join("data/dev/src")).unwrap();
        fs::write(sb.join("project"), "demo\n").unwrap();
        let copied = Command::new("cp")
            .arg("-a")
            .arg(&repo)
            .arg(sb.join("data/dev/src/demo"))
            .status()
            .unwrap();
        assert!(copied.success());
        m
    }

    fn repo(&self) -> PathBuf {
        self.at.join("data/dev/src/demo")
    }

    fn clone(&self) -> PathBuf {
        self.at.join("agents/demo-1/data/dev/src/demo")
    }

    fn sandbox(&self) -> Sandbox {
        Sandbox::find(&self.cfg, "demo-1").unwrap()
    }

    fn save(&self) -> Result<Saved, String> {
        let agent = Agent::new(&self.cfg, Path::new("/nonexistent.json")).unwrap();
        save(&agent, &self.sandbox())
    }

    /// git in `dir`, as the test (with an identity of its own): what it
    /// printed.
    fn git(&self, dir: &Path, args: &[&str]) -> String {
        let o = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8(o.stdout).unwrap()
    }

    fn show(&self, what: &str) -> String {
        self.git(&self.repo(), &["show", what])
    }
}

#[test]
fn its_branches_and_working_tree_come_back_as_refs() {
    let m = Machine::new();
    let clone = m.clone();
    m.git(&clone, &["checkout", "-qb", "feat"]);
    fs::write(clone.join("a"), "two\n").unwrap();
    m.git(&clone, &["commit", "-qam", "two"]);
    fs::write(clone.join("e"), "wip\n").unwrap();
    fs::write(clone.join(".gitignore"), "ignored\n").unwrap();
    fs::write(clone.join("ignored"), "no\n").unwrap();
    fs::write(clone.join(".git/info/exclude"), "excluded\n").unwrap();
    fs::write(clone.join("excluded"), "no\n").unwrap();

    assert_eq!(m.save(), Ok(Saved::Done));
    assert_eq!(m.show("refs/agents/demo-1/heads/feat:a"), "two\n");
    assert_eq!(m.show("refs/agents/demo-1/heads/main:a"), "one\n");
    assert_eq!(m.show("refs/agents/demo-1/worktree:e"), "wip\n");
    let files = m.git(
        &m.repo(),
        &["ls-tree", "--name-only", "refs/agents/demo-1/worktree"],
    );
    assert_eq!(files, ".gitignore\na\ne\n");
    // On top of its HEAD, by the agent.
    let parent = m.git(&m.repo(), &["rev-parse", "refs/agents/demo-1/worktree^"]);
    assert_eq!(
        parent,
        m.git(&m.repo(), &["rev-parse", "refs/agents/demo-1/heads/feat"])
    );
    let by = m.git(
        &m.repo(),
        &[
            "log",
            "-1",
            "--format=%an|%s",
            "refs/agents/demo-1/worktree",
        ],
    );
    assert!(
        by.starts_with("agent demo-1|agent demo-1: working tree as of "),
        "{by}"
    );
    // The real project's own branch and files didn't move.
    assert_eq!(
        m.git(&m.repo(), &["rev-parse", "--abbrev-ref", "HEAD"]),
        "main\n"
    );
    assert!(!m.repo().join("e").exists());
    // And the scratch repository is gone.
    let left: Vec<_> = fs::read_dir(m.at.join("agents/demo-1"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().into_string().unwrap())
        .filter(|n| n.starts_with("save."))
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn what_the_sandbox_wrote_into_its_git_config_never_runs() {
    let m = Machine::new();
    let clone = m.clone();
    let pwned = |what: &str| m.at.join(format!("pwned-{what}"));
    let touch = |what: &str| format!("touch {}", pwned(what).display());
    m.git(&clone, &["config", "core.fsmonitor", &touch("fsmonitor")]);
    m.git(&clone, &["config", "filter.x.clean", &touch("filter")]);
    m.git(
        &clone,
        &[
            "config",
            "core.hooksPath",
            &clone.join("hooks").display().to_string(),
        ],
    );
    fs::create_dir_all(clone.join("hooks")).unwrap();
    for hook in [
        "pre-commit",
        "post-commit",
        "reference-transaction",
        "post-index-change",
    ] {
        let h = clone.join("hooks").join(hook);
        fs::write(&h, format!("#!/bin/sh\n{}\n", touch(hook))).unwrap();
        Command::new("chmod").arg("+x").arg(&h).status().unwrap();
    }
    fs::write(clone.join(".gitattributes"), "* filter=x\n").unwrap();
    fs::write(clone.join("b"), "vm\n").unwrap();

    assert_eq!(m.save(), Ok(Saved::Done));
    assert_eq!(m.show("refs/agents/demo-1/worktree:b"), "vm\n");
    for what in [
        "fsmonitor",
        "filter",
        "pre-commit",
        "post-commit",
        "reference-transaction",
        "post-index-change",
    ] {
        assert!(!pwned(what).exists(), "{what} ran");
    }
}

#[test]
fn a_save_that_fails_says_so() {
    let m = Machine::new();
    fs::write(m.clone().join("e"), "wip\n").unwrap();
    // A file where the sandbox's refs go: the fetch can't write them.
    fs::create_dir_all(m.repo().join(".git/refs/agents")).unwrap();
    fs::write(m.repo().join(".git/refs/agents/demo-1"), "").unwrap();
    let e = m.save().unwrap_err();
    assert!(e.starts_with("git -C "), "{e}");
    assert!(e.contains(" fetch -q --no-write-fetch-head "), "{e}");
}

#[test]
fn a_branch_it_cant_bring_back_fails_the_save() {
    let m = Machine::new();
    let clone = m.clone();
    // Two branches git can't hold side by side, as only a sandbox that wrote
    // its own packed-refs could have them: the second can't be made.
    let head = m.git(&clone, &["rev-parse", "HEAD"]);
    let head = head.trim();
    fs::write(
        clone.join(".git/packed-refs"),
        format!("{head} refs/heads/a\n{head} refs/heads/a/b\n"),
    )
    .unwrap();
    let e = m.save().unwrap_err();
    assert!(e.contains(" update-ref refs/heads/a/b "), "{e}");
    // And nothing was fetched into the project.
    assert_eq!(m.git(&m.repo(), &["for-each-ref", "refs/agents"]), "");
}

#[test]
fn a_project_that_is_no_repository_of_its_own_has_nowhere_to_go() {
    let m = Machine::new();
    // A worktree's .git is a file pointing into the real repo on the host.
    let git = m.clone().join(".git");
    fs::remove_dir_all(&git).unwrap();
    fs::write(&git, "gitdir: /data/dev/src/demo/.git/worktrees/x\n").unwrap();
    assert_eq!(m.save(), Ok(Saved::NotARepository));

    let m = Machine::new();
    fs::remove_dir_all(m.repo().join(".git")).unwrap();
    assert_eq!(m.save(), Ok(Saved::NotARepository));
}

#[test]
fn a_repository_with_no_commits_yet_saves_its_files_as_a_root_commit() {
    let m = Machine::new();
    let clone = m.clone();
    fs::remove_dir_all(clone.join(".git")).unwrap();
    m.git(&clone, &["init", "-q", "-b", "main"]);
    assert_eq!(m.save(), Ok(Saved::Done));
    assert_eq!(m.show("refs/agents/demo-1/worktree:a"), "one\n");
    let parents = m.git(
        &m.repo(),
        &["log", "-1", "--format=%P", "refs/agents/demo-1/worktree"],
    );
    assert_eq!(parents, "\n");
}
