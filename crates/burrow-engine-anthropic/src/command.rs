//! Claude Code CLI command building and capability-grant flag rendering.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{Mode, api};

/// The Claude Code CLI executable name.
pub const PROGRAM: &str = "claude";

/// Disable claude.ai connectors (mail, calendar, drive, design tools) while
/// keeping the machine's own MCP servers.
pub const DISABLE_CLAUDEAI_MCP_ENV: (&str, &str) = ("ENABLE_CLAUDEAI_MCP_SERVERS", "false");

/// A stdio MCP server rendered into Claude Code's `--mcp-config`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct McpServer {
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}

/// Engine-agnostic job capability grants rendered into Claude Code's CLI flags
/// and execution environment.
#[derive(Clone, Debug, Default)]
pub struct Grants {
    pub env: BTreeMap<String, String>,
    pub inherit_environment: bool,
    /// `(path, writable)` grants; existing writable directories become `--add-dir`.
    pub paths: Vec<(PathBuf, bool)>,
    pub servers: BTreeMap<String, McpServer>,
}

/// Per-run options rendered into Claude Code's headless `stream-json` CLI flags.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunArgs {
    pub model: Option<String>,
    pub effort: Option<String>,
    pub session: Option<String>,
    /// Micro-dollars (`--max-budget-usd`), for `claude-api` admitted runs.
    pub max_budget_usd: Option<i64>,
    pub append_system_prompt: Option<String>,
}

/// Claude Code's flags for one headless turn: its stream format, effort,
/// resumed session, model, optional API budget, and appended system prompt.
pub fn run_args(opts: &RunArgs) -> Vec<String> {
    let mut args: Vec<String> = [
        "-p",
        "--output-format",
        "stream-json",
        "--verbose",
        "--disable-slash-commands",
    ]
    .map(String::from)
    .to_vec();
    if let Some(e) = &opts.effort {
        args.extend(["--effort".into(), e.clone()]);
    }
    if let Some(s) = &opts.session {
        args.extend(["--resume".into(), s.clone()]);
    }
    if let Some(m) = &opts.model {
        args.extend(["--model".into(), m.clone()]);
    }
    if let Some(budget) = opts.max_budget_usd {
        args.extend(api::budget_flag(budget));
    }
    if let Some(prompt) = &opts.append_system_prompt {
        args.extend(["--append-system-prompt".into(), prompt.clone()]);
    }
    args
}

/// Render capability grants into Claude Code CLI flags (`--settings`,
/// `--add-dir`, `--strict-mcp-config`, `--mcp-config`).
pub fn claude_flags(grants: &Grants) -> Vec<String> {
    let mut flags = vec![
        "--settings".into(),
        serde_json::json!({"env": grants.env}).to_string(),
    ];
    for (path, write) in &grants.paths {
        if *write && path.is_dir() {
            flags.extend(["--add-dir".into(), path.to_string_lossy().into_owned()]);
        }
    }
    let servers: BTreeMap<_, _> = grants
        .servers
        .iter()
        .map(|(name, server)| {
            (
                name,
                serde_json::json!({
                    "type": "stdio",
                    "command": server.command,
                    "args": server.args,
                    "env": server.env,
                }),
            )
        })
        .collect();
    flags.extend([
        "--strict-mcp-config".into(),
        "--mcp-config".into(),
        serde_json::json!({"mcpServers": servers}).to_string(),
    ]);
    flags
}

/// Build the `claude` [`Command`] with rendered capability flags, extra args,
/// environment isolation, and mode-specific credentials. The returned [`File`]
/// (when running in [`Mode::ClaudeApi`]) must outlive process launch/exec.
pub fn build_command(
    grants: &Grants,
    extra_args: &[String],
    mode: Mode,
    key_file: Option<&Path>,
) -> Result<(Command, Option<File>), api::Refused> {
    build_command_for(Path::new(PROGRAM), grants, extra_args, mode, None, key_file)
}

/// Like [`build_command`], with an explicit executable path and optional
/// in-memory API key value (from `credentialEnv`) for [`Mode::ClaudeApi`].
pub fn build_command_for(
    program: &Path,
    grants: &Grants,
    extra_args: &[String],
    mode: Mode,
    key_value: Option<&str>,
    key_file: Option<&Path>,
) -> Result<(Command, Option<File>), api::Refused> {
    let mut command = Command::new(program);
    if !grants.inherit_environment {
        command.env_clear();
    }
    command
        .args(claude_flags(grants))
        .args(extra_args)
        .envs(&grants.env);
    let key = api::credentials_from_source(&mut command, mode, key_value, key_file)?;
    Ok((command, key))
}

#[cfg(test)]
mod tests;
