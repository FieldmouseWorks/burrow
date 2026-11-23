//! Headless `antigravity` CLI invocation, credential isolation, and process-group
//! cancellation.

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use tempfile::TempDir;

use crate::config::{Config, CredentialMode, Effort, InputFormat, Mode, OutputFormat};
use crate::normalize::{RunError, TurnOutput, normalize_output};
use crate::schema::Schema;

/// Environment variables that must NEVER leak into the `antigravity` child:
/// GitHub tokens and other engines' credentials.
pub const STRIPPED_ENV: &[&str] = &[
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_TOKEN_SOCKET",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "ANTHROPIC_API_KEY",
];

/// Concrete parameters for one headless `antigravity` CLI invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invocation {
    pub workdir: PathBuf,
    pub prompt: String,
    pub model: Option<String>,
    pub effort: Option<Effort>,
    pub conversation: Option<String>,
    pub add_dirs: Vec<PathBuf>,
    pub json_schema: Option<String>,
    pub mode: Option<Mode>,
    pub dangerously_skip_permissions: bool,
    pub sandbox: bool,
    pub disable_slash_commands: bool,
    pub settings_file: Option<PathBuf>,
    pub output_format: OutputFormat,
    pub input_format: Option<InputFormat>,
    pub print_timeout_secs: Option<u64>,
}

impl Invocation {
    /// Build an invocation inheriting runtime defaults from `config`.
    pub fn from_config(
        config: &Config,
        workdir: PathBuf,
        prompt: String,
        model: Option<String>,
        effort: Option<Effort>,
        conversation: Option<String>,
    ) -> Self {
        Self {
            workdir,
            prompt,
            model,
            effort,
            conversation,
            add_dirs: config.add_dirs.clone(),
            json_schema: config.json_schema.clone(),
            mode: config.mode,
            dangerously_skip_permissions: config.dangerously_skip_permissions,
            sandbox: config.sandbox,
            disable_slash_commands: config.disable_slash_commands,
            settings_file: config.settings_file.clone(),
            output_format: config.output_format,
            input_format: config.input_format,
            print_timeout_secs: config.print_timeout_secs,
        }
    }
}

/// Render the verified headless `antigravity` CLI argument vector.
pub fn build_args(invocation: &Invocation) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        OsString::from("-p"),
        OsString::from("--output-format"),
        OsString::from(invocation.output_format.as_str()),
    ];
    if let Some(input_format) = invocation.input_format {
        args.push(OsString::from("--input-format"));
        args.push(OsString::from(input_format.as_str()));
    }
    if invocation.disable_slash_commands {
        args.push(OsString::from("--disable-slash-commands"));
    }
    if let Some(model) = &invocation.model {
        args.push(OsString::from("--model"));
        args.push(OsString::from(model));
    }
    if let Some(effort) = invocation.effort {
        args.push(OsString::from("--effort"));
        args.push(OsString::from(effort.as_str()));
    }
    if let Some(conversation) = &invocation.conversation {
        args.push(OsString::from("--conversation"));
        args.push(OsString::from(conversation));
    }
    for dir in &invocation.add_dirs {
        args.push(OsString::from("--add-dir"));
        args.push(dir.as_os_str().to_os_string());
    }
    if let Some(schema) = &invocation.json_schema {
        args.push(OsString::from("--json-schema"));
        args.push(OsString::from(schema));
    }
    if let Some(mode) = invocation.mode {
        args.push(OsString::from("--mode"));
        args.push(OsString::from(mode.as_str()));
    }
    if invocation.dangerously_skip_permissions {
        args.push(OsString::from("--dangerously-skip-permissions"));
    }
    if invocation.sandbox {
        args.push(OsString::from("--sandbox"));
    }
    if let Some(settings) = &invocation.settings_file {
        args.push(OsString::from("--settings"));
        args.push(settings.as_os_str().to_os_string());
    }
    if let Some(secs) = invocation.print_timeout_secs {
        args.push(OsString::from("--print-timeout"));
        args.push(OsString::from(secs.to_string()));
    }
    args
}

struct PreparedCredential {
    env_var: Option<(String, OsString)>,
    secret_value: Option<String>,
    _tempdir: Option<TempDir>,
}

fn prepare_credential(config: &Config) -> Result<PreparedCredential, String> {
    let target_name = config
        .target_credential_env
        .as_deref()
        .or(config.credential_env.as_deref());

    let raw_from_file = match &config.credential_file {
        Some(path) => match fs::read_to_string(path) {
            Ok(text) => Some(text.trim_end_matches(['\r', '\n']).to_owned()),
            Err(e) => {
                return Err(format!(
                    "failed to read configured credential file {}: {e}",
                    path.display()
                ));
            }
        },
        None => None,
    };

    let raw_from_env = config
        .credential_env
        .as_deref()
        .and_then(|name| std::env::var(name).ok())
        .map(|text| text.trim_end_matches(['\r', '\n']).to_owned())
        .filter(|s| !s.is_empty());

    let secret_value = raw_from_file.or(raw_from_env);

    match config.credential_mode {
        CredentialMode::Env => {
            let env_var = match (target_name, &secret_value) {
                (Some(name), Some(val)) => Some((name.to_owned(), OsString::from(val))),
                _ => None,
            };
            Ok(PreparedCredential {
                env_var,
                secret_value,
                _tempdir: None,
            })
        }
        CredentialMode::File => {
            let Some(name) = target_name else {
                return Ok(PreparedCredential {
                    env_var: None,
                    secret_value,
                    _tempdir: None,
                });
            };
            if let Some(path) = &config.credential_file {
                return Ok(PreparedCredential {
                    env_var: Some((name.to_owned(), path.as_os_str().to_os_string())),
                    secret_value,
                    _tempdir: None,
                });
            }
            let Some(val) = &secret_value else {
                return Ok(PreparedCredential {
                    env_var: None,
                    secret_value: None,
                    _tempdir: None,
                });
            };
            let dir = tempfile::Builder::new()
                .prefix("antigravity-cred-")
                .tempdir()
                .map_err(|e| format!("failed to create isolated credential dir: {e}"))?;
            fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700))
                .map_err(|e| format!("failed to secure credential dir: {e}"))?;
            let file_path = dir.path().join("credential");
            fs::write(&file_path, val)
                .map_err(|e| format!("failed to write isolated credential file: {e}"))?;
            fs::set_permissions(&file_path, fs::Permissions::from_mode(0o600))
                .map_err(|e| format!("failed to secure credential file: {e}"))?;
            Ok(PreparedCredential {
                env_var: Some((name.to_owned(), file_path.into_os_string())),
                secret_value,
                _tempdir: Some(dir),
            })
        }
    }
}

/// Run one headless `antigravity` invocation in its own process group.
///
/// When `stop()` returns `true` or the outer deadline elapses, the entire child
/// process group is killed and reaped immediately via `burrow-process`.
pub fn run_invocation(
    config: &Config,
    invocation: &Invocation,
    stop: &dyn Fn() -> bool,
) -> TurnOutput {
    // A schema that can't be held to is refused before the child runs: never
    // "any JSON will do".
    let schema = match invocation
        .json_schema
        .as_deref()
        .map(Schema::compile)
        .transpose()
    {
        Ok(schema) => schema,
        Err(why) => {
            return TurnOutput {
                error: Some(RunError::ProcessFailed {
                    exit_code: None,
                    signal: None,
                    message: why,
                }),
                ..TurnOutput::default()
            };
        }
    };
    let prepared = match prepare_credential(config) {
        Ok(p) => p,
        Err(why) => {
            return TurnOutput {
                error: Some(RunError::ProcessFailed {
                    exit_code: None,
                    signal: None,
                    message: why,
                }),
                ..TurnOutput::default()
            };
        }
    };

    let mut command = Command::new(&config.binary);
    command
        .args(build_args(invocation))
        .current_dir(&invocation.workdir);

    for var in STRIPPED_ENV {
        command.env_remove(var);
    }
    if let Some(src_env) = &config.credential_env
        && config.target_credential_env.as_deref() != Some(src_env.as_str())
    {
        command.env_remove(src_env);
    }
    if let Some((key, val)) = &prepared.env_var {
        command.env(key, val);
    }

    let deadline = Instant::now() + Duration::from_secs(config.deadline_secs);
    let result = burrow_process::output_unless(
        &mut command,
        Some(invocation.prompt.as_bytes()),
        deadline,
        config.max_output_bytes,
        stop,
    );

    let secrets: Vec<&str> = prepared.secret_value.as_deref().into_iter().collect();

    match result {
        Ok(output) => normalize_output(
            invocation.output_format,
            &output.stdout,
            &output.stderr,
            output.status.code(),
            output.status.signal(),
            schema.as_ref(),
            &secrets,
        ),
        Err(why) if why == "command was stopped" => TurnOutput {
            error: Some(RunError::Cancelled),
            ..TurnOutput::default()
        },
        Err(why) if why == "command exceeded its time limit" => TurnOutput {
            error: Some(RunError::TimedOut),
            ..TurnOutput::default()
        },
        Err(why) => TurnOutput {
            error: Some(RunError::ProcessFailed {
                exit_code: None,
                signal: None,
                message: crate::normalize::scrub(&why, &secrets),
            }),
            ..TurnOutput::default()
        },
    }
}
