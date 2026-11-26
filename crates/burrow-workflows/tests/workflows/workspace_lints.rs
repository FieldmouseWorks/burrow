//! The Cargo workspace's clippy ratchet: every member inherits
//! `[workspace.lints]`, and the workspace's lint settings cause CI's
//! `cargo clippy --all-targets -- -D warnings` (`crates-rust-clippy` in
//! `lib/project.nix`) to reject a new over-length function.

use std::fs;
use std::path::Path;
use std::process::Command;

use crate::harness::{output, read, repo_file};

/// Every member named in `crates/Cargo.toml`'s `[workspace] members` array.
fn workspace_members(manifest: &str) -> Vec<String> {
    let mut in_members = false;
    let mut members = Vec::new();
    for line in manifest.lines() {
        let trimmed = line.trim();
        if !in_members {
            if trimmed.starts_with("members") && trimmed.contains('[') {
                in_members = true;
            }
            continue;
        }
        if trimmed.starts_with(']') {
            break;
        }
        if let Some(quoted) = trimmed.strip_prefix('"')
            && let Some(name) = quoted.split('"').next()
            && !name.is_empty()
        {
            members.push(name.to_owned());
        }
    }
    members
}

/// Every `[workspace.lints...]` table copied verbatim from `crates/Cargo.toml`.
fn workspace_lints_section(manifest: &str) -> String {
    let mut kept = String::new();
    let mut in_lints = false;
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_lints = trimmed.starts_with("[workspace.lints");
        }
        if in_lints {
            kept.push_str(line);
            kept.push('\n');
        }
    }
    kept
}

/// `cargo` stripped of the outer test build's Cargo/rustc environment so the
/// fixture builds cleanly on its own.
fn clean_cargo(dir: &Path) -> Command {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut cmd = Command::new(cargo);
    cmd.current_dir(dir);
    for (name, _) in std::env::vars_os() {
        let text = name.to_string_lossy();
        if text.starts_with("CARGO_") && text != "CARGO_HOME"
            || matches!(
                &*text,
                "RUSTC_WRAPPER" | "RUSTC_WORKSPACE_WRAPPER" | "RUSTFLAGS"
            )
        {
            cmd.env_remove(&name);
        }
    }
    cmd
}

fn run_clippy(dir: &Path) -> (bool, String) {
    let mut cmd = clean_cargo(dir);
    cmd.args([
        "clippy",
        "--offline",
        "--all-targets",
        "--",
        "-D",
        "warnings",
    ]);
    let out = output(cmd);
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), combined)
}

fn write_fixture(root: &Path, lints: &str, body: &str) {
    let member = root.join("probe/src");
    fs::create_dir_all(&member).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        format!("[workspace]\nmembers = [\"probe\"]\nresolver = \"3\"\n\n{lints}"),
    )
    .unwrap();
    let clippy_toml = repo_file("crates/clippy.toml");
    if clippy_toml.is_file() {
        burrow_test::copy_file(&clippy_toml, root.join("clippy.toml"));
    }
    fs::write(
        root.join("probe/Cargo.toml"),
        "[package]\nname = \"probe\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n[lints]\nworkspace = true\n",
    )
    .unwrap();
    fs::write(member.join("lib.rs"), body).unwrap();
}

#[test]
fn every_workspace_member_inherits_workspace_lints() {
    let manifest = read(&repo_file("crates/Cargo.toml"));
    let members = workspace_members(&manifest);
    assert!(!members.is_empty(), "crates/Cargo.toml lists no members");
    for member in members {
        let path = repo_file(&format!("crates/{member}/Cargo.toml"));
        let text = read(&path);
        let mut in_lints = false;
        let mut inherits = false;
        for line in text.lines().map(str::trim) {
            if line.starts_with('[') {
                in_lints = line == "[lints]";
            } else if in_lints && line.replace(' ', "") == "workspace=true" {
                inherits = true;
            }
        }
        assert!(
            inherits,
            "{}: missing `[lints] workspace = true`",
            path.display()
        );
    }
}

#[test]
fn workspace_clippy_rejects_over_length_function() {
    let manifest = read(&repo_file("crates/Cargo.toml"));
    let lints = workspace_lints_section(&manifest);
    assert!(
        !lints.is_empty(),
        "crates/Cargo.toml has no [workspace.lints] section"
    );

    let dir = tempfile::tempdir().unwrap();
    write_fixture(
        dir.path(),
        &lints,
        "pub fn short(x: u32) -> u32 {\n    x + 1\n}\n",
    );
    let (passed, said) = run_clippy(dir.path());
    assert!(passed, "short fixture failed clippy:\n{said}");

    let mut long_fn = String::from("pub fn too_long(mut x: u32) -> u32 {\n");
    for i in 0..110u32 {
        long_fn.push_str(&format!("    x = x.wrapping_add({i});\n"));
    }
    long_fn.push_str("    x\n}\n");
    fs::write(dir.path().join("probe/src/lib.rs"), long_fn).unwrap();

    let (passed, said) = run_clippy(dir.path());
    assert!(
        !passed && said.contains("clippy::too_many_lines"),
        "expected clippy::too_many_lines failure on over-length function:\n{said}"
    );
}
