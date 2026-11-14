//! burrow-guard as the hook runs it: a config, the hook's JSON on stdin,
//! and nothing or a decision on stdout.

use std::fs;
use std::io::{ErrorKind, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

mod common;

/// What `burrow-guard CONFIG` prints for `input`, outside a proj job.
fn hook(config: &str, input: &str) -> String {
    hook_in(None, config, input)
}

/// What `burrow-guard CONFIG` prints for `input`, in the proj job `job`.
fn hook_in(job: Option<&str>, config: &str, input: &str) -> String {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("guard.json");
    fs::write(&path, config).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_burrow-guard"));
    command.env_remove("PROJ_JOB");
    if let Some(job) = job {
        command.env("PROJ_JOB", job);
    }
    let mut child = command
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

const CONFIG: &str = r#"{"git": "git", "zfs": "/no-such-zfs", "systemdRun": "/no-such-systemd-run", "snapshotDatasets": [], "askAfterSeconds": 8}"#;

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

/// A Claude Code under proj (PROJ_JOB) is a job's, whose `first` label
/// the guard asks about; anyone else's isn't.
#[test]
fn a_jobs_claude_is_asked_before_it_puts_first_on() {
    let config = r#"{"git": "git", "zfs": "/no-such-zfs", "systemdRun": "/no-such-systemd-run", "snapshotDatasets": [], "askAfterSeconds": 8, "firstLabel": "first"}"#;
    let input = r#"{"tool_name": "Bash", "tool_input": {"command": "gh issue edit 5 --add-label first"}, "cwd": "/"}"#;
    let out: serde_json::Value =
        serde_json::from_str(&hook_in(Some("web-1"), config, input)).unwrap();
    assert_eq!(out["hookSpecificOutput"]["permissionDecision"], "ask");
    let reason = out["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .unwrap();
    assert!(reason.starts_with("Puts the `first` label on"), "{reason}");
    assert_eq!(hook_in(Some(""), config, input), "");
    assert_eq!(hook(config, input), "");
    // No label to keep: nothing to ask.
    assert_eq!(hook_in(Some("web-1"), CONFIG, input), "");
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
            r#"{"git": "git", "zfs": "zfs", "systemdRun": "/no-such-systemd-run", "snapshotDatasets": [], "askAfterSeconds": 8, "extra": 1}"#,
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
    let git = common::stand_ins().join("hang");
    let config = format!(
        r#"{{"git": "{}", "zfs": "/no-such-zfs", "systemdRun": "/no-such-systemd-run", "snapshotDatasets": [], "askAfterSeconds": 1}}"#,
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
