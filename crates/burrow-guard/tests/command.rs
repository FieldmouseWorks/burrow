//! burrow-guard as the hook runs it: a config, the hook's JSON on stdin,
//! and nothing or a decision on stdout.

use std::fs;
use std::io::{ErrorKind, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

mod common;

/// What `burrow-guard CONFIG` prints for `input`, outside a proj job.
fn hook(config: &str, input: &str) -> String {
    hook_in(Job::Empty, config, input)
}

/// Where the guard runs, as PROJ_JOB says.
#[derive(Clone, Copy)]
enum Job<'a> {
    /// In the proj job it names.
    Named(&'a str),
    /// Set, and empty: no job's.
    Empty,
    /// Not in its environment, but in the engine's that runs it, as for an
    /// engine that gives its hook an environment of its own.
    Engines(&'a str),
    /// Not in its environment, nor anywhere it runs under.
    Absent,
}

/// What `burrow-guard CONFIG` prints for `input`, as `job` says.
fn hook_in(job: Job<'_>, config: &str, input: &str) -> String {
    guard_in(job, None, config, input, &[])
}

/// What the guard prints for `input`, Claude Code's or (`codex_user`)
/// Codex's, as `job` says, with `env` set too.
fn guard_in(
    job: Job<'_>,
    codex_user: Option<&str>,
    config: &str,
    input: &str,
    env: &[(&str, &std::path::Path)],
) -> String {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("guard.json");
    fs::write(&path, config).unwrap();
    let guard = env!("CARGO_BIN_EXE_burrow-guard");
    let mut command = match job {
        // A shell that stays, as the engine that runs the hook.
        Job::Engines(id) => {
            let mut sh = Command::new("sh");
            sh.args(["-c", r#"env -u PROJ_JOB "$0" "$@"; exit $?"#, guard])
                .env("PROJ_JOB", id);
            sh
        }
        _ => Command::new(guard),
    };
    match job {
        Job::Named(id) => {
            command.env("PROJ_JOB", id);
        }
        Job::Empty => {
            command.env("PROJ_JOB", "");
        }
        Job::Absent => {
            command.env_remove("PROJ_JOB");
        }
        Job::Engines(_) => {}
    }
    if let Some(user) = codex_user {
        command.args(["--codex-user", user]);
    }
    let mut child = command
        .arg(&path)
        .env("XDG_RUNTIME_DIR", dir.path())
        .envs(env.iter().copied())
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

const FIRST: &str = r#"{"git": "git", "zfs": "/no-such-zfs", "systemdRun": "/no-such-systemd-run", "snapshotDatasets": [], "askAfterSeconds": 8, "firstLabel": "first"}"#;
const PUTS_FIRST_ON: &str = r#"{"tool_name": "Bash", "tool_input": {"command": "gh issue edit 5 --add-label first"}, "cwd": "/"}"#;

/// The decision and its reason, if the guard printed one.
fn decision(out: &str) -> Option<(String, String)> {
    let out: serde_json::Value = serde_json::from_str(out).ok()?;
    let said = &out["hookSpecificOutput"];
    Some((
        said["permissionDecision"].as_str()?.to_owned(),
        said["permissionDecisionReason"].as_str()?.to_owned(),
    ))
}

/// A Claude Code under proj (PROJ_JOB) is a job's, whose `first` label
/// the guard asks about; anyone else's isn't.
#[test]
fn a_jobs_claude_is_asked_before_it_puts_first_on() {
    for job in [Job::Named("web-1"), Job::Engines("web-1")] {
        let (decision, reason) = decision(&hook_in(job, FIRST, PUTS_FIRST_ON)).unwrap();
        assert_eq!(decision, "ask");
        assert!(reason.starts_with("Puts the `first` label on"), "{reason}");
    }
    assert_eq!(hook_in(Job::Empty, FIRST, PUTS_FIRST_ON), "");
    assert_eq!(hook_in(Job::Engines(""), FIRST, PUTS_FIRST_ON), "");
    // Outside every job (in one, this test's own runs under it).
    if std::env::var_os("PROJ_JOB").is_none() {
        assert_eq!(hook_in(Job::Absent, FIRST, PUTS_FIRST_ON), "");
    }
    // No label to keep: nothing to ask.
    assert_eq!(hook_in(Job::Named("web-1"), CONFIG, PUTS_FIRST_ON), "");
}

/// Codex's guard (`--codex-user`) refuses a job's `first`, under its own
/// PROJ_JOB or its engine's, and leaves the owner's own Codex, which proj
/// didn't start, to put it on; its own refusals hold in either.
#[test]
fn only_a_jobs_codex_is_refused_the_label() {
    let me = burrow_guard::config::current_user().expect("the test's user has a name");
    let codex = |job| guard_in(job, Some(&me), FIRST, PUTS_FIRST_ON, &[]);
    for job in [Job::Named("web-1"), Job::Engines("web-1")] {
        let (decision, reason) = decision(&codex(job)).unwrap();
        assert_eq!(decision, "deny");
        assert!(reason.starts_with("Puts the `first` label on"), "{reason}");
    }
    assert_eq!(codex(Job::Empty), "");
    if std::env::var_os("PROJ_JOB").is_none() {
        assert_eq!(codex(Job::Absent), "");
    }
    let merge = r#"{"tool_name": "Bash", "tool_input": {"command": "gh pr merge 5"}, "cwd": "/"}"#;
    let (decision, _) = decision(&guard_in(Job::Empty, Some(&me), FIRST, merge, &[])).unwrap();
    assert_eq!(decision, "deny");
}

/// A curlrc where the job's curl would read it, under any of the places
/// curl looks, may make any curl a write that puts `first` on: a job's
/// curl is asked about, unless it starts with `-q`, which keeps curl off it.
#[test]
fn a_jobs_curl_with_a_default_config_is_asked_about() {
    let home = tempfile::tempdir().unwrap();
    fs::write(
        home.path().join(".curlrc"),
        "request = POST\ndata = \"{\\\"labels\\\":[\\\"first\\\"]}\"\n",
    )
    .unwrap();
    let curl = |cmd: &str| {
        format!(r#"{{"tool_name": "Bash", "tool_input": {{"command": "{cmd}"}}, "cwd": "/"}}"#)
    };
    let read = curl("curl https://api.github.com/repos/o/r/issues/5/labels");
    let kept_off = curl("curl -q https://api.github.com/repos/o/r/issues/5/labels");
    for var in ["HOME", "CURL_HOME", "XDG_CONFIG_HOME"] {
        let env = [(var, home.path())];
        let (decision, reason) =
            decision(&guard_in(Job::Named("web-1"), None, FIRST, &read, &env)).unwrap();
        assert_eq!(decision, "ask", "{var}");
        assert!(reason.starts_with("Puts the `first` label on"), "{reason}");
        assert_eq!(
            guard_in(Job::Named("web-1"), None, FIRST, &kept_off, &env),
            "",
            "{var}"
        );
        assert_eq!(guard_in(Job::Empty, None, FIRST, &read, &env), "", "{var}");
    }
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
