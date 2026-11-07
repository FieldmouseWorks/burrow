//! Codex's apply_patch deletes and replaces files with no command to read:
//! the guard reads the patch, as Codex hands its hook one (its own tool's
//! input, or a command that runs `apply_patch`, which Codex applies
//! itself), and takes the undo snapshot first, or refuses with none.

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;

use burrow_guard::config::{Config, current_user};
use burrow_guard::{Guard, Hook, Input, Namespace};

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

fn hook(tool: &str, command: &str, cwd: &Path) -> Hook {
    Hook {
        tool_name: Some(tool.to_owned()),
        tool_input: Some(Input {
            command: Some(command.to_owned()),
        }),
        cwd: Some(cwd.to_string_lossy().into_owned()),
    }
}

fn patch(body: &str) -> String {
    format!("*** Begin Patch\n{body}\n*** End Patch\n")
}

/// Whether the guard took a snapshot before `tool`'s `command` at `cwd`,
/// and what it said.
fn snapshotted(tool: &str, command: &str, cwd: &Path) -> (bool, Option<String>) {
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        git: PathBuf::from("git"),
        zfs: zfs(dir.path()),
        systemd_run: PathBuf::from("/no-such-systemd-run"),
        snapshot_datasets: vec!["pool/a".to_owned()],
        ask_after_seconds: 8,
    };
    let guard = Guard {
        config: &config,
        codex: true,
        mark: dir.path().join("mark"),
        namespace: Namespace::Machine,
    };
    let said = guard.check(&hook(tool, command, cwd), SystemTime::now());
    (dir.path().join("zfs.log").exists(), said)
}

#[test]
fn a_patch_that_deletes_or_replaces_a_file_is_snapshotted_first() {
    // The checkout, and a scratch worktree beside it: wherever the job works.
    let work = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    for at in [work.path(), scratch.path()] {
        fs::write(at.join("kept.txt"), "work\n").unwrap();
        let elsewhere = scratch.path().join("kept.txt");
        for body in [
            "*** Delete File: kept.txt".to_owned(),
            format!("*** Delete File: {}", elsewhere.display()),
            "*** Update File: a.txt\n*** Move to: kept.txt\n@@\n-a\n+b".to_owned(),
            "*** Add File: kept.txt\n+new".to_owned(),
        ] {
            let p = patch(&body);
            for (tool, command) in [
                ("apply_patch", p.clone()),
                ("Bash", format!("apply_patch <<'EOF'\n{p}EOF")),
            ] {
                let (taken, said) = snapshotted(tool, &command, at);
                assert!(taken, "no snapshot before {tool}: {command}");
                assert_eq!(said, None, "{tool}: {command}");
            }
        }
    }
}

#[test]
fn a_patch_that_only_adds_or_edits_isnt() {
    let work = tempfile::tempdir().unwrap();
    for body in [
        "*** Add File: new.txt\n+new",
        "*** Update File: a.txt\n@@\n-a\n+b",
    ] {
        let (taken, said) = snapshotted("apply_patch", &patch(body), work.path());
        assert!(!taken, "a snapshot before: {body}");
        assert_eq!(said, None);
    }
}

/// `burrow-guard --codex-user ME CONFIG` on an apply_patch, as Codex runs it.
fn codex_hook(config: &str, patch: &str, cwd: &Path) -> String {
    let me = current_user().expect("this user has a name");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("guard.json");
    fs::write(&path, config).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_burrow-guard"))
        .args(["--codex-user", &me])
        .arg(&path)
        .env("XDG_RUNTIME_DIR", dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let input = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "apply_patch",
        "tool_input": {"command": patch},
        "cwd": cwd,
    });
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn no_snapshot_no_patch() {
    let dir = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    fs::write(work.path().join("kept.txt"), "work\n").unwrap();
    let config = serde_json::json!({
        "git": "git",
        "zfs": zfs(dir.path()),
        "snapshotDatasets": ["pool/a"],
        "systemdRun": "/no-such-systemd-run",
        "askAfterSeconds": 8,
    })
    .to_string();
    fs::write(dir.path().join("fail"), "").unwrap();
    let out = codex_hook(&config, &patch("*** Delete File: kept.txt"), work.path());
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let o = &v["hookSpecificOutput"];
    assert_eq!(o["permissionDecision"], "deny", "{out}");
    assert!(
        o["permissionDecisionReason"]
            .as_str()
            .unwrap()
            .contains("undo snapshot"),
        "{out}"
    );
    // An edit has no snapshot to need.
    let edit = patch("*** Update File: kept.txt\n@@\n-work\n+play");
    assert_eq!(codex_hook(&config, &edit, work.path()), "");
}

/// A patch a command hands Codex lands where the command has moved to:
/// the guard reads `cd DIR && apply_patch` as Codex does, and any other
/// move (or one it can't read) as anywhere, so an Add File there counts.
#[test]
fn a_commands_patch_is_read_where_it_lands() {
    let work = tempfile::tempdir().unwrap();
    let tree = tempfile::tempdir().unwrap();
    fs::create_dir(work.path().join("sub")).unwrap();
    fs::write(work.path().join("sub").join("kept.txt"), "work\n").unwrap();
    fs::write(tree.path().join("kept.txt"), "work\n").unwrap();
    let p = patch("*** Add File: kept.txt\n+new");
    let heredoc = format!("apply_patch <<'EOF'\n{p}EOF");
    let t = tree.path().display();
    // Nothing at kept.txt in the starting directory: each of these
    // replaces a file only because of where it moved first.
    for command in [
        format!("cd sub && {heredoc}"),
        format!("cd 'sub' && {heredoc}"),
        format!("cd {t} && {heredoc}"),
        format!("cd \"{t}\" && {heredoc}"),
        format!("cd \"$PROJ_SCRATCH/tree\" && {heredoc}"),
        format!("cd ~/tree && {heredoc}"),
        format!("cd sub; {heredoc}"),
        format!("pushd sub && {heredoc}"),
        format!("(cd sub && {heredoc})"),
        format!("bash -c \"cd sub && {heredoc}\""),
        format!("sh -c 'cd sub; apply_patch' <<'EOF'\n{p}EOF"),
        format!("env -C sub apply_patch <<'EOF'\n{p}EOF"),
        format!("{heredoc}\ncd sub && {heredoc}"),
        format!("cd sub && apply_patch <<'EOF' && true\n{p}EOF"),
    ] {
        let (taken, said) = snapshotted("Bash", &command, work.path());
        assert!(taken, "no snapshot before: {command}");
        assert_eq!(said, None, "{command}");
    }
    // Where it's known and nothing is there, an Add File only adds.
    for command in [
        heredoc.clone(),
        format!("cd {t}/new && {heredoc}"),
        format!("apply_patch <<EOF\n{p}EOF\n"),
        format!("  cd sub/.. && apply_patch << \"PATCH\"\n{p}PATCH\n"),
    ] {
        fs::create_dir_all(tree.path().join("new")).unwrap();
        let (taken, said) = snapshotted("Bash", &command, work.path());
        assert!(!taken, "a snapshot before: {command}");
        assert_eq!(said, None, "{command}");
    }
}
