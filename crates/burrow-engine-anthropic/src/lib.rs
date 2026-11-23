//! Anthropic engine logic for Claude Code CLI (`claude` subscription and
//! `claude-api` credit modes): CLI flags and command assembly, stream JSONL
//! parsing, usage/login/API refusal classification, and credit meter admission.

pub mod api;
pub mod claude;
pub mod command;
pub mod limit;
pub mod stream;

/// Which Anthropic authentication mode Claude Code runs under.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// Subscription login (`claude`).
    #[default]
    Claude,
    /// API credit key (`claude-api`).
    ClaudeApi,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::ClaudeApi => burrow_settings::credit::ENGINE,
        }
    }
}
