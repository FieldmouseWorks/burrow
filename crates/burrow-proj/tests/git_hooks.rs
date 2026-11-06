//! Real hooks on a scratch worktree and a remote whose default is not main.
use std::{
    fs,
    os::unix::fs::symlink,
    path::Path,
    process::{Command, Output},
};

fn git(dir: &Path, hooks: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_COUNT", "4")
        .env("GIT_CONFIG_KEY_0", "core.hooksPath")
        .env("GIT_CONFIG_VALUE_0", hooks)
        .env("GIT_CONFIG_KEY_1", "user.name")
        .env("GIT_CONFIG_VALUE_1", "Worker")
        .env("GIT_CONFIG_KEY_2", "user.email")
        .env("GIT_CONFIG_VALUE_2", "worker@example.test")
        .env("GIT_CONFIG_KEY_3", "commit.gpgsign")
        .env("GIT_CONFIG_VALUE_3", "false")
        .output()
        .unwrap()
}
fn succeeds(out: Output) -> String {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

#[test]
fn worktree_commits_are_marked_and_default_branch_rewrites_and_deletes_are_refused() {
    let d = tempfile::tempdir().unwrap();
    let hooks = d.path().join("hooks");
    fs::create_dir(&hooks).unwrap();
    for name in ["commit-msg", "pre-push"] {
        symlink(env!("CARGO_BIN_EXE_burrow-proj-git-hook"), hooks.join(name)).unwrap();
    }
    let clone = d.path().join("clone");
    fs::create_dir(&clone).unwrap();
    let run = |dir: &Path, args: &[&str]| git(dir, &hooks, args);
    succeeds(run(&clone, &["init", "-b", "trunk"]));
    succeeds(run(&clone, &["commit", "--allow-empty", "-m", "initial"]));
    let initial = succeeds(run(&clone, &["rev-parse", "HEAD"]));
    let remote = d.path().join("remote");
    succeeds(run(
        d.path(),
        &[
            "clone",
            "--bare",
            clone.to_str().unwrap(),
            remote.to_str().unwrap(),
        ],
    ));
    // Without the hook, this bare remote permits deleting its default branch.
    succeeds(run(
        &remote,
        &["config", "receive.denyDeleteCurrent", "false"],
    ));
    succeeds(run(
        &clone,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    ));
    let tree = d.path().join("scratch");
    succeeds(run(
        &clone,
        &["worktree", "add", "-b", "work", tree.to_str().unwrap()],
    ));
    succeeds(run(&tree, &["commit", "--allow-empty", "-m", "changed"]));
    succeeds(run(
        &tree,
        &["commit", "--amend", "--allow-empty", "--no-edit"],
    ));
    let message = succeeds(run(&tree, &["log", "-1", "--format=%B"]));
    assert_eq!(message.matches("Engine: codex").count(), 1);
    succeeds(run(&tree, &["push", "origin", "HEAD:work"]));
    succeeds(run(&tree, &["push", "origin", "HEAD:trunk"])); // fast-forward is allowed
    let tip = succeeds(run(&remote, &["rev-parse", "trunk"]));
    succeeds(run(&tree, &["reset", "--hard", &initial]));
    for args in [
        ["push", "--force", "origin", "HEAD:trunk"].as_slice(),
        &["push", "origin", ":trunk"],
    ] {
        let refused = run(&tree, args);
        assert!(!refused.status.success());
        assert!(
            String::from_utf8_lossy(&refused.stderr)
                .contains("cannot delete or force-push refs/heads/trunk")
        );
        assert_eq!(succeeds(run(&remote, &["rev-parse", "trunk"])), tip);
    }
}
