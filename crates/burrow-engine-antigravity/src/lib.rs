//! Isolated Antigravity engine surface and Conary `conary-plugin v5` `job`
//! plugin.
//!
//! This crate shares no provider-specific code with Anthropic and does not
//! depend on `burrow-proj`. It owns:
//!
//! - Runtime configuration ([`config`]) with no hardcoded model IDs or
//!   interactive/T3 userdata paths.
//! - Headless `antigravity` CLI invocation ([`cli`]) with credential isolation,
//!   GitHub-token stripping, `--json-schema` support, and process-group
//!   cancellation via `burrow-process`.
//! - Output normalization and Antigravity-local quota/login refusal
//!   classification ([`normalize`]).
//! - Conary `conary-plugin v5` `describe` and `job` handling ([`plugin`]).

pub mod cli;
pub mod config;
pub mod normalize;
pub mod plugin;
pub mod schema;

pub use cli::{Invocation, STRIPPED_ENV, build_args, run_invocation};
pub use config::{
    Config, CredentialMode, DEFAULT_CREDENTIAL_HOME_PATH, EFFORTS, ENGINE, Effort, HOST_HOME_ENV,
    InputFormat, Mode, ModelSpec, OutputFormat, validate_credential_home_path,
};
pub use normalize::{Refusal, RunError, TurnOutput, Usage, classify_refusal, normalize_output};
pub use plugin::{answer, answer_cancellable, describe, is_conformance_job};
pub use schema::Schema;

#[cfg(test)]
mod tests;
