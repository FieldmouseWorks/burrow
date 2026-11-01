//! burrow-guard as the hook runs it: a config, the hook's JSON on stdin,
//! and nothing or a decision on stdout.

use std::fs;
use std::io::{ErrorKind, Write};
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// What `burrow-guard CONFIG` prints for `input`.
fn hook(config: &str, input: &str) -> String {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("guard.json");
    fs::write(&path, config).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_burrow-guard"))
        .arg(&path)
        .env("XDG_RUNTIME_DIR", dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    // A config it can't read answers without reading its input, and may be
    // gone by now: what it answers is the test's to judge.
    if let Err(e) = child.stdin.take().unwrap().write_all(input.as_bytes()) {
        assert_eq!(e.kind(), ErrorKind::BrokenPipe, "{e}");
    }
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap()
}

const CONFIG: &str =
    r#"{"git": "git", "zfs": "/no-such-zfs", "snapshotDatasets": [], "askAfterSeconds": 8}"#;

#[test]
fn a_command_it_lets_through_gets_no_answer() {
    let input =
        r#"{"session_id": "x", "tool_name": "Bash", "tool_input": {"command": "ls"}, "cwd": "/"}"#;
    assert_eq!(hook(CONFIG, input), "");
}

#[test]
fn a_question_is_the_hooks_ask_decision() {
    let input =
        r#"{"tool_name": "Bash", "tool_input": {"command": "efibootmgr -o 1"}, "cwd": "/"}"#;
    let out: serde_json::Value = serde_json::from_str(&hook(CONFIG, input)).unwrap();
    assert_eq!(
        out,
        serde_json::json!({"hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "ask",
            "permissionDecisionReason": "Changes UEFI boot entries (R2: a bad one can stop the box booting).",
        }})
    );
}

/// A guard that can't read what it guards asks, rather than let the
/// command through unguarded.
#[test]
fn a_guard_that_cant_check_asks() {
    let ls = r#"{"tool_name": "Bash", "tool_input": {"command": "ls"}}"#;
    for (config, input) in [
        (CONFIG, "not json"),
        (r#"{"git": "git"}"#, ls),
        (
            r#"{"git": "git", "zfs": "zfs", "snapshotDatasets": [], "askAfterSeconds": 8, "extra": 1}"#,
            ls,
        ),
    ] {
        let out: serde_json::Value = serde_json::from_str(&hook(config, input)).unwrap();
        let reason = out["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap();
        assert!(
            reason.starts_with("The guard couldn't check this command ("),
            "{reason}"
        );
        assert_eq!(out["hookSpecificOutput"]["permissionDecision"], "ask");
    }
}

/// A guard stuck past its deadline asks, as the hook's timeout would let the
/// command run unchecked.
#[test]
fn a_guard_that_takes_too_long_asks() {
    let dir = tempfile::tempdir().unwrap();
    let git = dir.path().join("git");
    fs::write(&git, "#!/bin/sh\nexec sleep 30\n").unwrap();
    fs::set_permissions(&git, fs::Permissions::from_mode(0o755)).unwrap();
    let config = format!(
        r#"{{"git": "{}", "zfs": "/no-such-zfs", "snapshotDatasets": [], "askAfterSeconds": 1}}"#,
        git.display()
    );
    let input = r#"{"tool_name": "Bash", "tool_input": {"command": "git push -f"}, "cwd": "/"}"#;
    let started = Instant::now();
    let out: serde_json::Value = serde_json::from_str(&hook(&config, input)).unwrap();
    assert!(started.elapsed() < Duration::from_secs(20));
    assert_eq!(
        out["hookSpecificOutput"]["permissionDecisionReason"],
        "The guard couldn't check this command (no answer after 1 s), so it asks."
    );
    assert_eq!(out["hookSpecificOutput"]["permissionDecision"], "ask");
}
