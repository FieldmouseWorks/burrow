//! The guard as Codex's managed hook for proj's Codex jobs: it refuses
//! where Claude Code's asks, takes the same undo snapshot, and refuses what
//! would make a commit the job's hooks never mark or a push past them.

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;

use burrow_guard::config::{Config, current_user};
use burrow_guard::{Guard, Hook, Input};

fn bash(cmd: &str, cwd: &Path) -> Hook {
    Hook {
        tool_name: Some("Bash".to_owned()),
        tool_input: Some(Input {
            command: Some(cmd.to_owned()),
        }),
        cwd: Some(cwd.to_string_lossy().into_owned()),
    }
}

/// What the guard says about `cmd`, as Codex's (`codex`) or Claude's.
fn says(cmd: &str, codex: bool) -> Option<String> {
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        git: PathBuf::from("git"),
        zfs: PathBuf::from("/no-such-zfs"),
        snapshot_datasets: Vec::new(),
        ask_after_seconds: 8,
    };
    let guard = Guard {
        config: &config,
        codex,
        mark: dir.path().join("mark"),
    };
    guard.check(&bash(cmd, dir.path()), SystemTime::now())
}

#[test]
fn what_hides_a_commit_from_the_jobs_hooks_is_refused() {
    for cmd in [
        "git commit --no-verify -m change",
        "git commit -nm change",
        "git -C /work commit -a -n -m change",
        "git -c core.hooksPath=/dev/null commit -m change",
        "git -c core.HOOKSPATH=/tmp/none commit -m change",
        "git config core.hooksPath /dev/null",
        "GIT_CONFIG_COUNT=0 git commit -m change",
        "unset GIT_CONFIG_COUNT; git commit -m change",
        "env -u GIT_CONFIG_COUNT git commit -m change",
        "env --unset=GIT_CONFIG_COUNT git commit -m change",
        "env -i PATH=/bin git commit -m change",
        "env - git commit -m change",
        "exec -c git commit -m change",
        "git push --no-verify origin work",
        "git cherry-pick abc123",
        "git revert --no-edit HEAD",
        "git rebase origin/main",
        "git -C /work rebase -i HEAD~2",
        "git pull --rebase origin main",
        "git pull -r",
        "git commit-tree HEAD^{tree} -m change",
        "git am change.patch",
        "git send-pack origin work",
        "bash -c 'git revert HEAD'",
        "echo $(git cherry-pick abc)",
        "gh api -X PUT repos/o/r/contents/README.md -f message=change -f content=eA==",
        "gh api repos/o/r/git/commits -f message=change -f tree=abc",
        "gh api -X PATCH repos/o/r/git/refs/heads/work -F force=true -f sha=abc",
        "gh api --method DELETE repos/o/r/git/refs/heads/work",
        "gh api repos/o/r/merges -f base=work -f head=other",
    ] {
        assert!(says(cmd, true).is_some(), "let through: {cmd}");
        assert_eq!(
            says(cmd, false),
            None,
            "Claude's guard said something: {cmd}"
        );
    }
}

#[test]
fn ordinary_work_goes_through() {
    for cmd in [
        "git commit -m change",
        "git commit -am change",
        "git commit --amend --no-edit",
        "git commit -m subject -m 'with -x in it'",
        "git merge origin/main",
        "git pull origin main",
        "git push origin work",
        "git log --format=%n",
        "git status",
        "env FOO=1 cargo build",
        "cargo test --workspace",
        "rg -n 'fn read() -> String' crates",
        "git rebased-note",
        "env FOO=1 ls -lu",
        "gh api repos/o/r/contents/README.md",
        "gh api repos/o/r/git/refs/heads/main",
        "gh api -X GET repos/o/r/git/commits/abc -f per_page=1",
        "gh api repos/o/r/pulls/1/comments -f body=thanks",
        "gh pr view 9 --json headRefOid",
    ] {
        assert_eq!(says(cmd, true), None, "refused: {cmd}");
    }
}

/// push.rs reads the push itself, so turning the hooks off doesn't get a
/// default branch's rewrite through: Claude's guard asks, Codex's refuses.
#[test]
fn a_default_branch_rewrite_is_caught_whatever_the_hooks() {
    let branch = "main";
    for cmd in [
        format!("git push origin --force {branch} --no-verify"),
        format!("git -c core.hooksPath=/dev/null push -f origin {branch}"),
        format!("GIT_CONFIG_COUNT=0 git push origin +{branch}"),
        format!("git push origin :{branch}"),
        format!("git push --delete origin {branch}"),
    ] {
        let claude = says(&cmd, false).unwrap_or_default();
        assert!(claude.contains(branch), "{cmd}: {claude}");
        let codex = says(&cmd, true).unwrap_or_default();
        assert!(codex.contains(&claude), "{cmd}: {codex}");
    }
}

/// `burrow-guard --codex-user USER CONFIG`, as Codex runs it, on `command`.
fn hook(user: &str, config: &str, command: &str) -> String {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("guard.json");
    fs::write(&path, config).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_burrow-guard"))
        .args(["--codex-user", user])
        .arg(&path)
        .env("XDG_RUNTIME_DIR", dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let input = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
        "cwd": "/",
    });
    // One it doesn't guard, or can't read the config of, may answer
    // without reading its input.
    let _ = child
        .stdin
        .take()
        .unwrap()
        .write_all(input.to_string().as_bytes());
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap()
}

/// A zfs that logs what it's asked, and fails when `fail` is there.
fn zfs(dir: &Path) -> PathBuf {
    let zfs = dir.join("zfs");
    fs::write(
        &zfs,
        format!(
            "#!/bin/sh\necho \"$@\" >> '{0}/zfs.log'\n[ ! -e '{0}/fail' ] || {{ echo no >&2; exit 1; }}\n",
            dir.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&zfs, fs::Permissions::from_mode(0o755)).unwrap();
    zfs
}

fn decision(out: &str) -> (String, String) {
    let v: serde_json::Value = serde_json::from_str(out).unwrap();
    let o = &v["hookSpecificOutput"];
    (
        o["permissionDecision"].as_str().unwrap().to_owned(),
        o["permissionDecisionReason"].as_str().unwrap().to_owned(),
    )
}

#[test]
fn codexs_hook_snapshots_first_and_refuses_with_no_undo() {
    let me = current_user().expect("this user has a name");
    let dir = tempfile::tempdir().unwrap();
    let config = serde_json::json!({
        "git": "git",
        "zfs": zfs(dir.path()),
        "snapshotDatasets": ["pool/a"],
        "askAfterSeconds": 8,
    })
    .to_string();
    // Snapshotted, then let through.
    assert_eq!(hook(&me, &config, "rm -rf build"), "");
    let log = fs::read_to_string(dir.path().join("zfs.log")).unwrap();
    assert!(log.starts_with("snapshot pool/a@pre-"), "{log}");
    // No snapshot, no undo: refused, not asked (Codex runs what's asked).
    fs::write(dir.path().join("fail"), "").unwrap();
    for cmd in [
        "rm -rf build",
        "git clean -fdx",
        "git reset --hard",
        "echo x > out",
    ] {
        let (decision, reason) = decision(&hook(&me, &config, cmd));
        assert_eq!(decision, "deny", "{cmd}");
        assert!(reason.contains("undo snapshot"), "{cmd}: {reason}");
    }
    // A one-way door the guard reads, and a hidden commit.
    assert_eq!(decision(&hook(&me, &config, "efibootmgr -o 1")).0, "deny");
    assert_eq!(decision(&hook(&me, &config, "git revert HEAD")).0, "deny");
    // Another user's Codex (the reviewer's) isn't the jobs'.
    assert_eq!(hook("no-such-user", &config, "efibootmgr -o 1"), "");
}

#[test]
fn codexs_hook_that_cant_check_refuses() {
    let me = current_user().expect("this user has a name");
    let (decision, reason) = decision(&hook(&me, r#"{"git": "git"}"#, "ls"));
    assert_eq!(decision, "deny");
    assert!(
        reason.starts_with("The guard couldn't check this command ("),
        "{reason}"
    );
}
