//! Claude Code's PreToolUse guard, the reversibility classes made a hook:
//!
//! - R0, undoable in seconds: allowed, silently (the default).
//! - R1, undoable only with a primitive: the primitive first, then allowed.
//!   A command that deletes or rewrites files gets a ZFS snapshot of the
//!   undo datasets (undo.rs).
//! - R2, a one-way door: the owner is asked. The ones a pattern can name
//!   are `permissions.ask` in modules/claude.nix, which ask even under
//!   bypassPermissions; this asks for the ones a pattern can't: a
//!   force-push or delete of a default branch (push.rs), and firmware and
//!   Hetzner Robot calls with no undo (doors.rs).
//!
//! The command is read as text, line by line, so one nested in quotes
//! (`ssh host '…'`, `sh -c "…"`) counts the same as one typed bare.
//!
//! The same guard is Codex's managed PreToolUse hook for proj's Codex jobs
//! (`--codex-user`, modules/claude/guard.nix). There no one is asked: the
//! pinned Codex runs a command whose hook asks, so every question is a
//! refusal, and it refuses what would hide a commit from the job's hooks
//! (codex.rs). Codex's apply_patch, which deletes and replaces files with no
//! command, gets the undo snapshot too.

mod ansi_c;
mod codex;
pub mod config;
mod doors;
mod push;
mod shell;
mod undo;
mod walk;

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use regex::Regex;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::Config;

/// The hook's input, as Claude Code or Codex sends it on stdin: a Bash
/// command, or Codex's apply_patch with its patch as `command`. The rest of
/// its fields are no business of the guard's.
#[derive(Debug, Default, Deserialize)]
pub struct Hook {
    pub tool_name: Option<String>,
    pub tool_input: Option<Input>,
    /// Where the command runs.
    pub cwd: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Input {
    pub command: Option<String>,
}

pub struct Guard<'a> {
    pub config: &'a Config,
    /// Whether it guards a Codex job, which gets codex.rs's refusals too.
    pub codex: bool,
    /// When the last undo snapshot was taken, as its mtime: one round per
    /// five seconds is plenty for a burst of commands.
    pub mark: PathBuf,
}

impl Guard<'_> {
    /// What the hook says about `hook`'s command: `None` lets it run, a
    /// question asks the owner first.
    #[must_use]
    pub fn check(&self, hook: &Hook, now: SystemTime) -> Option<String> {
        let tool = hook.tool_name.as_deref();
        if !matches!(tool, Some("Bash" | "apply_patch")) {
            return None;
        }
        let cmd = hook
            .tool_input
            .as_ref()
            .and_then(|i| i.command.as_deref())
            .unwrap_or_default();
        let cwd = Path::new(hook.cwd.as_deref().filter(|c| !c.is_empty()).unwrap_or("."));
        let mut asks = Vec::new();
        // The undo first, so a command the owner lets through at a question
        // has its snapshot too. A patch, Codex's own tool's or one a command
        // hands Codex, is read for the files it deletes or replaces.
        let destroys =
            undo::patch_destroys(cmd, cwd) || (tool == Some("Bash") && undo::destroys(cmd));
        if !self.config.snapshot_datasets.is_empty() && destroys {
            asks.extend(undo::snapshot(
                &self.config.zfs,
                &self.config.snapshot_datasets,
                &self.mark,
                now,
            ));
        }
        // A patch is only files: what it says isn't run.
        if tool == Some("Bash") {
            asks.extend(push::asks(cmd, cwd, &self.config.git));
            asks.extend(doors::asks(cmd));
            if self.codex {
                asks.extend(codex::refusals(cmd));
            }
        }
        asks.dedup();
        (!asks.is_empty()).then(|| asks.join(" "))
    }
}

/// The hook's answer that asks the owner `question`.
#[must_use]
pub fn ask(question: &str) -> Value {
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "ask",
            "permissionDecisionReason": question,
        }
    })
}

/// The hook's answer that refuses the command for `reason`: Codex's, whose
/// job has no one to ask.
#[must_use]
pub fn deny(reason: &str) -> Value {
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }
    })
}

/// Whether any line of `cmd` matches `re`, as `grep -E` reads it.
fn any_line(re: &Regex, cmd: &str) -> bool {
    cmd.split('\n').any(|line| re.is_match(line))
}

/// One of the guard's own patterns, which are fixed: a bad one is a bug the
/// tests catch.
fn pattern(re: &str) -> Regex {
    Regex::new(re).expect("the guard's patterns are valid")
}
