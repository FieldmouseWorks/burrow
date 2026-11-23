//! Runtime configuration for the Antigravity engine plugin.
//!
//! All model IDs, executable paths, credential locations, and sandbox flags are
//! supplied at runtime (from a JSON config file or environment variables); no
//! model ID or user-specific path is hardcoded here.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use conary_core::plugin::{Capabilities, Model, Offers, Sandbox};
use serde::{Deserialize, Serialize};

/// Stable engine identity reported in Conary plugin `by.engine`.
pub const ENGINE: &str = "antigravity";

/// Efforts accepted by the Antigravity CLI (`--effort`).
pub const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// Environment variable pointing to a JSON [`Config`] file.
pub const CONFIG_ENV: &str = "BURROW_ANTIGRAVITY_CONFIG";

/// Optional environment overrides for ad-hoc or fixture invocations.
pub const BIN_ENV: &str = "BURROW_ANTIGRAVITY_BIN";
pub const JSON_SCHEMA_ENV: &str = "BURROW_ANTIGRAVITY_JSON_SCHEMA";
pub const PRINT_TIMEOUT_ENV: &str = "BURROW_ANTIGRAVITY_PRINT_TIMEOUT_SECS";

/// Default outer wall-clock deadline (seconds) for one headless invocation.
pub const DEFAULT_DEADLINE_SECS: u64 = 1800;

/// Default maximum bytes captured per stdout/stderr pipe (16 MiB).
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;

/// Effort level passed to `antigravity --effort`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl Effort {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            "xhigh" => Some(Self::Xhigh),
            "max" => Some(Self::Max),
            _ => None,
        }
    }
}

/// Permission/execution mode passed to `antigravity --mode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    AcceptEdits,
    Auto,
    Plan,
}

impl Mode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AcceptEdits => "accept-edits",
            Self::Auto => "auto",
            Self::Plan => "plan",
        }
    }
}

/// Output format passed to `antigravity --output-format`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OutputFormat {
    #[default]
    StreamJson,
    Json,
    Text,
}

impl OutputFormat {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StreamJson => "stream-json",
            Self::Json => "json",
            Self::Text => "text",
        }
    }
}

/// Input format passed to `antigravity --input-format`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InputFormat {
    Text,
    StreamJson,
}

impl InputFormat {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::StreamJson => "stream-json",
        }
    }
}

/// How the isolated auth credential is handed to `antigravity`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CredentialMode {
    /// Pass the credential string directly in `target_credential_env` (or `credential_env`).
    #[default]
    Env,
    /// Pass a path to a private `0600` credential file in `target_credential_env` (or `credential_env`).
    File,
}

/// Runtime-configured model offer advertised in Conary `describe`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSpec {
    pub id: String,
    #[serde(default = "default_efforts")]
    pub efforts: Vec<String>,
    #[serde(default)]
    pub context_tokens: Option<u64>,
    #[serde(default)]
    pub max_output_tokens: Option<u64>,
}

fn default_efforts() -> Vec<String> {
    EFFORTS.iter().map(|e| (*e).to_owned()).collect()
}

impl ModelSpec {
    pub fn to_conary(&self) -> Model {
        Model {
            id: self.id.clone(),
            efforts: self.efforts.clone(),
            context_tokens: self.context_tokens,
            max_output_tokens: self.max_output_tokens,
        }
    }
}

/// Full runtime configuration for the Antigravity plugin executable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Path (or executable name) of the `antigravity` CLI binary.
    pub binary: PathBuf,
    /// Runtime-configured models advertised by `describe`.
    pub models: Vec<ModelSpec>,
    /// Default model when a job request does not specify one.
    pub default_model: Option<String>,
    /// Default effort when a job request does not specify one.
    pub default_effort: Option<Effort>,
    /// Environment variable carrying the isolated credential into the plugin.
    pub credential_env: Option<String>,
    /// Environment variable name to expose to `antigravity` (defaults to `credential_env`).
    pub target_credential_env: Option<String>,
    /// Optional explicit credential file path (independent of interactive/T3 userdata).
    pub credential_file: Option<PathBuf>,
    /// Whether `antigravity` expects the credential value or a file path in the env var.
    pub credential_mode: CredentialMode,
    /// Optional `--settings` path passed to `antigravity`.
    pub settings_file: Option<PathBuf>,
    /// Optional `--mode` (`accept-edits`, `auto`, `plan`).
    pub mode: Option<Mode>,
    /// Pass `--dangerously-skip-permissions` (intended only inside an isolated microVM).
    pub dangerously_skip_permissions: bool,
    /// Pass `--sandbox` to `antigravity`.
    pub sandbox: bool,
    /// Pass `--disable-slash-commands` (default true for headless jobs).
    pub disable_slash_commands: bool,
    /// Output format requested from `antigravity`.
    pub output_format: OutputFormat,
    /// Optional `--input-format` requested from `antigravity`.
    pub input_format: Option<InputFormat>,
    /// Additional directories passed via repeatable `--add-dir`.
    pub add_dirs: Vec<PathBuf>,
    /// Optional `--json-schema` (inline JSON string or file path) for structured output.
    pub json_schema: Option<String>,
    /// Optional `--print-timeout` (seconds) passed to `antigravity`.
    pub print_timeout_secs: Option<u64>,
    /// Outer wall-clock deadline (seconds) enforced on the child process group.
    pub deadline_secs: u64,
    /// Maximum stdout/stderr bytes captured per stream.
    pub max_output_bytes: usize,
    /// Whether `describe` declares the `network` capability.
    pub network: bool,
    /// Whether `describe` declares the `mcp` capability.
    pub mcp: bool,
    /// Devices declared in `describe` (for example `["kvm"]`).
    pub devices: Vec<String>,
    /// Usage meter names declared in `describe`.
    pub meters: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            binary: PathBuf::from("antigravity"),
            models: Vec::new(),
            default_model: None,
            default_effort: None,
            credential_env: None,
            target_credential_env: None,
            credential_file: None,
            credential_mode: CredentialMode::Env,
            settings_file: None,
            mode: None,
            dangerously_skip_permissions: false,
            sandbox: false,
            disable_slash_commands: true,
            output_format: OutputFormat::StreamJson,
            input_format: None,
            add_dirs: Vec::new(),
            json_schema: None,
            print_timeout_secs: None,
            deadline_secs: DEFAULT_DEADLINE_SECS,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            network: true,
            mcp: false,
            devices: Vec::new(),
            meters: Vec::new(),
        }
    }
}

impl Config {
    /// Load from an explicit path, `BURROW_ANTIGRAVITY_CONFIG`, or defaults,
    /// then apply targeted env overrides and validate.
    pub fn load(path: Option<&Path>) -> Result<Self, String> {
        let env_path = std::env::var_os(CONFIG_ENV).map(PathBuf::from);
        let config_path = path.or(env_path.as_deref());
        let mut cfg = match config_path {
            Some(at) => Self::read(at)?,
            None => Self::default(),
        };
        if let Some(bin) = std::env::var_os(BIN_ENV)
            && !bin.is_empty()
        {
            cfg.binary = PathBuf::from(bin);
        }
        if let Ok(schema) = std::env::var(JSON_SCHEMA_ENV)
            && !schema.trim().is_empty()
        {
            cfg.json_schema = Some(schema);
        }
        if let Ok(timeout) = std::env::var(PRINT_TIMEOUT_ENV)
            && !timeout.trim().is_empty()
        {
            let secs = timeout
                .trim()
                .parse::<u64>()
                .map_err(|e| format!("{PRINT_TIMEOUT_ENV}: invalid seconds: {e}"))?;
            cfg.print_timeout_secs = Some(secs);
        }
        cfg.validate()?;
        Ok(cfg)
    }

    /// Read and validate a JSON configuration file.
    pub fn read(path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let cfg: Self = serde_json::from_slice(&bytes)
            .map_err(|e| format!("{}: invalid config JSON: {e}", path.display()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Validate all fields so `describe` and invocation never emit invalid wire values.
    pub fn validate(&self) -> Result<(), String> {
        if self.binary.as_os_str().is_empty() {
            return Err("antigravity binary path must not be empty".to_owned());
        }
        if self.deadline_secs == 0 {
            return Err("deadline_secs must be positive".to_owned());
        }
        if self.max_output_bytes == 0 {
            return Err("max_output_bytes must be positive".to_owned());
        }
        if self.print_timeout_secs == Some(0) {
            return Err("print_timeout_secs must be positive when set".to_owned());
        }
        let mut seen_models = BTreeSet::new();
        for model in &self.models {
            if model.id.trim().is_empty() {
                return Err("model id must not be blank".to_owned());
            }
            if !seen_models.insert(model.id.as_str()) {
                return Err(format!("duplicate model id {:?}", model.id));
            }
            let mut seen_efforts = BTreeSet::new();
            for effort in &model.efforts {
                if Effort::parse(effort).is_none() {
                    return Err(format!(
                        "model {:?} lists unsupported effort {:?} (expected one of {:?})",
                        model.id, effort, EFFORTS
                    ));
                }
                if !seen_efforts.insert(effort.as_str()) {
                    return Err(format!(
                        "model {:?} lists duplicate effort {:?}",
                        model.id, effort
                    ));
                }
            }
        }
        if let Some(default_model) = &self.default_model {
            let Some(spec) = self.models.iter().find(|m| m.id == *default_model) else {
                return Err(format!(
                    "default_model {:?} is not in configured models",
                    default_model
                ));
            };
            if let Some(effort) = self.default_effort
                && !spec.efforts.iter().any(|e| e == effort.as_str())
            {
                return Err(format!(
                    "default_effort {:?} is not offered by default_model {:?}",
                    effort.as_str(),
                    default_model
                ));
            }
        }
        validate_unique_nonblank("devices", &self.devices)?;
        validate_unique_nonblank("meters", &self.meters)?;
        Ok(())
    }

    /// Conary `Offers` advertised by `describe`.
    pub fn offers(&self) -> Offers {
        Offers {
            models: self.models.iter().map(ModelSpec::to_conary).collect(),
            capabilities: Capabilities {
                sandbox: Sandbox::Needs,
                network: self.network,
                devices: self.devices.clone(),
                mcp: self.mcp,
            },
            meters: self.meters.clone(),
        }
    }
}

fn validate_unique_nonblank(field: &str, values: &[String]) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    for value in values {
        if value.trim().is_empty() {
            return Err(format!("{field} must not contain blank entries"));
        }
        if !seen.insert(value.as_str()) {
            return Err(format!("{field} contains duplicate entry {value:?}"));
        }
    }
    Ok(())
}
