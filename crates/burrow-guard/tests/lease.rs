//! Two jobs on one PR branch (FieldmouseWorks/burrow#1072): one pushes a
//! fix, the other fetches, rebuilds the branch from its own stale copy and
//! force-pushes. The guard refuses every push that would drop the fix, and
//! what it lets through git refuses until the fix is taken in.

use std::path::Path;
use std::process::{Command, Output, Stdio};

use burrow_guard::config::Config;
use burrow_guard::{Guard, Hook, Input, Namespace};

fn git_at(dir: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .args([
            "-c",
            "user.name=Invented",
            "-c",
            "user.email=invented@example.invalid",
        ])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = git_at(dir, args);
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// `cmd` run by the shell in `dir`, as the job's Bash tool would.
fn sh(dir: &Path, cmd: &str) -> Output {
    Command::new("sh")
        .args(["-c", cmd])
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

/// What the guard refuses `cmd` for, run in `cwd`.
fn refusal(cmd: &str, cwd: &Path) -> Option<String> {
    let config = Config {
        git: "git".into(),
        zfs: "/no-such-zfs".into(),
        systemd_run: "/no-such-systemd-run".into(),
        snapshot_datasets: Vec::new(),
        ask_after_seconds: 8,
    };
    let guard = Guard {
        config: &config,
        codex: false,
        mark: cwd.join("mark"),
        namespace: Namespace::Machine,
    };
    guard.refusal(&Hook {
        tool_name: Some("Bash".to_owned()),
        tool_input: Some(Input {
            command: Some(cmd.to_owned()),
        }),
        cwd: Some(cwd.to_string_lossy().into_owned()),
    })
}

/// Whether the remote's `pr` holds `commit`.
fn holds(remote: &Path, commit: &str) -> bool {
    git_at(
        remote,
        &["merge-base", "--is-ancestor", commit, "refs/heads/pr"],
    )
    .status
    .success()
}

#[test]
fn a_force_push_rebuilt_from_a_stale_branch_cant_drop_another_jobs_fix() {
    let root = tempfile::tempdir().unwrap();
    let remote = root.path().join("remote.git");
    let (stale, fixer) = (root.path().join("stale"), root.path().join("fixer"));
    git(
        root.path(),
        &["init", "-q", "--bare", "-b", "main", "remote.git"],
    );
    git(root.path(), &["clone", "-q", "remote.git", "stale"]);
    git(&stale, &["commit", "-q", "--allow-empty", "-m", "base"]);
    git(&stale, &["push", "-q", "origin", "main"]);
    git(&stale, &["checkout", "-q", "-b", "pr"]);
    git(
        &stale,
        &["commit", "-q", "--allow-empty", "-m", "the change"],
    );
    git(&stale, &["push", "-q", "-u", "origin", "pr"]);
    let seen = git(&stale, &["rev-parse", "HEAD"]);

    // The other job fixes the PR's findings and pushes.
    git(
        root.path(),
        &["clone", "-q", "-b", "pr", "remote.git", "fixer"],
    );
    git(&fixer, &["commit", "-q", "--allow-empty", "-m", "the fix"]);
    git(&fixer, &["push", "-q", "origin", "pr"]);
    let fix = git(&fixer, &["rev-parse", "HEAD"]);

    // This one fetches (main moved), and rebuilds pr from its own copy.
    git(&stale, &["fetch", "-q", "origin"]);
    git(
        &stale,
        &["commit", "-q", "--amend", "--allow-empty", "-m", "rebuilt"],
    );

    // Each way to force it, which git would let through over the fix: the
    // guard refuses them all.
    for cmd in [
        "git push -f origin pr",
        "git push --force",
        "git push origin +pr",
        "git push --force-with-lease origin pr",
        "git push --force-with-lease=pr origin pr",
        "git push --force-with-lease=pr:origin/pr origin pr",
        "git push --force-with-lease --force-if-includes -f origin pr",
        "git push --force-with-lease --force-if-includes origin +pr",
    ] {
        assert!(refusal(cmd, &stale).is_some(), "{cmd}");
        let dry = cmd.replacen("git push", "git push --dry-run --porcelain", 1);
        let out = sh(&stale, &dry);
        assert!(
            out.status.success()
                && String::from_utf8_lossy(&out.stdout).contains("+\trefs/heads/pr"),
            "{cmd} would have been refused by git anyway: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // What the guard lets through, git refuses: the fix stays.
    for cmd in [
        "git push --force-with-lease --force-if-includes origin pr",
        "git push --force-with-lease=pr --force-if-includes origin HEAD:pr",
        &format!("git push --force-with-lease=pr:{seen} origin pr"),
    ] {
        assert_eq!(refusal(cmd, &stale), None, "{cmd}");
        assert!(!sh(&stale, cmd).status.success(), "{cmd}");
        assert!(holds(&remote, &fix), "{cmd} dropped the fix");
    }

    // Once the fix is taken in, the leased push goes through, fix and all.
    git(&stale, &["reset", "-q", "--hard", "origin/pr"]);
    git(
        &stale,
        &[
            "commit",
            "-q",
            "--amend",
            "--allow-empty",
            "-m",
            "fix, amended",
        ],
    );
    git(&stale, &["rebase", "-q", "origin/main"]);
    let cmd = "git push --force-with-lease --force-if-includes origin pr";
    assert_eq!(refusal(cmd, &stale), None);
    let out = sh(&stale, cmd);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        git(&remote, &["rev-parse", "refs/heads/pr"]),
        git(&stale, &["rev-parse", "HEAD"])
    );
}
