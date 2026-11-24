//! Anthropic engine logic for Claude Code CLI (`claude` subscription and
//! `claude-api` credit modes): CLI flags and command assembly, stream JSONL
//! parsing, usage/login/API refusal classification, credit meter admission,
//! and the `conary-plugin v5` `job` plugin implementation.

use serde::{Deserialize, Serialize};

pub mod api;
pub mod claude;
pub mod command;
pub mod limit;
pub mod plugin;
pub mod stream;

/// Which Anthropic authentication mode Claude Code runs under.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    /// Subscription login (`claude`).
    #[default]
    #[serde(rename = "claude")]
    Claude,
    /// API credit key (`claude-api`).
    #[serde(rename = "claude-api")]
    ClaudeApi,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::ClaudeApi => burrow_settings::credit::ENGINE,
        }
    }

    /// Default environment variable carrying this mode's credential when run in
    /// burrow's shared plugin sandbox (`burrow.agents.engines.NAME.credentialEnv`).
    pub fn default_credential_env(self) -> &'static str {
        match self {
            Self::Claude => "CLAUDE_CODE_OAUTH_TOKEN",
            Self::ClaudeApi => "ANTHROPIC_API_KEY",
        }
    }
}
