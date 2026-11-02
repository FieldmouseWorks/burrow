//! What's new upstream: every flake input, and Claude Code's latest release,
//! brought into the clone and committed as a bump.

use std::fs;
use std::process::{Command, Stdio};

use serde_json::Value;

use crate::{Broke, Config};

/// Claude Code, which a run bumps beside the flake's inputs when the flake
/// pins it.
pub const CLAUDE: &str = "claude-code";

/// The flake's inputs, as nix reads its lock.
pub fn inputs(cfg: &Config) -> Result<Vec<String>, Broke> {
    let out = cfg
        .run_as_user("nix")
        .args(["flake", "metadata", "--json"])
        .arg(&cfg.work)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| Broke(format!("nix flake metadata: {e}")))?;
    if !out.status.success() {
        return Err(Broke(format!("nix flake metadata: {}", out.status)));
    }
    let meta: Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| Broke(format!("nix flake metadata: {e}")))?;
    let Some(inputs) = meta["locks"]["nodes"]["root"]["inputs"].as_object() else {
        return Err(Broke("nix flake metadata: no root inputs".into()));
    };
    // In order, as jq's keys has them.
    let mut inputs: Vec<String> = inputs.keys().cloned().collect();
    inputs.sort();
    Ok(inputs)
}

/// `input` brought up to what's newest, in the clone: false when it couldn't
/// find out, or bring in, what's new. What nix says goes to logs/update.log.
pub fn update_one(cfg: &Config, input: &str) -> bool {
    if input == CLAUDE {
        return claude(cfg);
    }
    let Ok(log) = fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(cfg.state.join("logs/update.log"))
    else {
        return false;
    };
    cfg.run_as_user("nix")
        .args(["flake", "update", input, "--flake"])
        .arg(&cfg.work)
        .stdin(Stdio::null())
        .stderr(log)
        .status()
        .is_ok_and(|s| s.success())
}

/// Claude Code's latest release, in the flake's manifest: its manifest
/// fetched, checked and put in the clone as the user's. A flake that
/// doesn't pin Claude Code has nothing to bring in.
fn claude(cfg: &Config) -> bool {
    let Some(manifest) = &cfg.manifest else {
        return true;
    };
    let Some(latest) = fetch(cfg, &format!("{}/latest", cfg.releases), 30, None) else {
        return false;
    };
    let latest = latest.trim_end_matches('\n');
    // It goes into a URL.
    if latest.is_empty()
        || !latest
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+'))
    {
        return false;
    }
    let pinned = fs::read(cfg.work.join(manifest))
        .ok()
        .and_then(|t| serde_json::from_slice::<Value>(&t).ok());
    if pinned.as_ref().and_then(|m| m["version"].as_str()) == Some(latest) {
        return true;
    }
    let fetched = cfg.state.join("manifest.json");
    let url = format!("{}/{latest}/manifest.zst.json", cfg.releases);
    if fetch(cfg, &url, 60, Some(&fetched)).is_none() {
        return false;
    }
    let whole = fs::read(&fetched)
        .ok()
        .and_then(|t| serde_json::from_slice::<Value>(&t).ok())
        .is_some_and(|m| truthy(&m["version"]) && truthy(&m["platforms"]["linux-x64"]["checksum"]));
    whole
        && Command::new(&cfg.tools.install)
            .args(["-o", &cfg.user, "-g", &cfg.group, "-m", "644"])
            .arg(&fetched)
            .arg(cfg.work.join(manifest))
            .status()
            .is_ok_and(|s| s.success())
}

/// What curl fetched from `url` within `secs` (into `to`, when given):
/// nothing when it didn't.
fn fetch(cfg: &Config, url: &str, secs: u32, to: Option<&std::path::Path>) -> Option<String> {
    let mut c = Command::new(&cfg.tools.curl);
    c.args(["-fsS", "--max-time", &secs.to_string(), url]);
    if let Some(to) = to {
        c.arg("-o").arg(to);
    }
    let out = c.stdin(Stdio::null()).output().ok()?;
    if !out.status.success() {
        eprint!("{}", String::from_utf8_lossy(&out.stderr));
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Truthy, as jq -e reads a value: anything but null and false.
fn truthy(v: &Value) -> bool {
    !matches!(v, Value::Null | Value::Bool(false))
}

/// What moved since `base`: "nixpkgs cd4ea8d -> 1a2b3c4" for each of
/// `inputs` whose locked commit did, and Claude Code's version.
pub fn describe(cfg: &Config, inputs: &[String], base: &str) -> Vec<String> {
    let old_lock = shown(cfg, base, "flake.lock");
    let new_lock = read(&cfg.work.join("flake.lock"));
    let mut moved = Vec::new();
    for i in inputs {
        let (old, new) = (locked(&old_lock, i), locked(&new_lock, i));
        if old != new {
            moved.push(format!("{i} {old} -> {new}"));
        }
    }
    if let Some(manifest) = &cfg.manifest {
        let version = |m: &Value| match &m["version"] {
            Value::String(s) => s.clone(),
            v => v.to_string(),
        };
        let (old, new) = (
            version(&shown(cfg, base, manifest)),
            version(&read(&cfg.work.join(manifest))),
        );
        if old != new {
            moved.push(format!("{CLAUDE} {old} -> {new}"));
        }
    }
    moved
}

/// What moved, as a run says it.
pub fn said(moved: &[String]) -> String {
    moved.join(", ")
}

/// The commit `input` is locked to in `lock`, short, as `git log --oneline`
/// shows one: nothing for an input that isn't locked to one, or that
/// follows another's (which says what moved).
fn locked(lock: &Value, input: &str) -> String {
    let node = lock["nodes"]["root"]["inputs"][input]
        .as_str()
        .unwrap_or("");
    let rev = lock["nodes"][node]["locked"]["rev"].as_str().unwrap_or("");
    rev.chars().take(7).collect()
}

fn read(path: &std::path::Path) -> Value {
    fs::read(path)
        .ok()
        .and_then(|t| serde_json::from_slice(&t).ok())
        .unwrap_or(Value::Null)
}

/// `file` at `commit`, as JSON: null when it isn't there or isn't JSON.
fn shown(cfg: &Config, commit: &str, file: &str) -> Value {
    cfg.git_hushed(&["show", &format!("{commit}:{file}")])
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null)
}

/// What's been brought in, committed on HEAD as a bump that says what moved:
/// false when nothing was.
pub fn commit(cfg: &Config, inputs: &[String]) -> bool {
    if cfg.git(&["diff", "--quiet"]).is_ok() {
        return false;
    }
    let what = said(&describe(cfg, inputs, "HEAD"));
    cfg.git(&[
        "commit",
        "-qam",
        &format!("deps: update {what}"),
        "-m",
        &format!(
            "Pushed by {} after the build, the VM tests and the health check passed.",
            cfg.update
        ),
    ])
    .is_ok()
}

#[cfg(test)]
#[path = "bump_tests.rs"]
mod tests;
