//! Headless `antigravity` CLI invocation, credential isolation, and process-group
//! cancellation.

use std::ffi::OsString;
use std::fs::{self, DirBuilder};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::process::ExitStatusExt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use tempfile::TempDir;

use crate::config::{
    Config, CredentialMode, Effort, HOST_HOME_ENV, InputFormat, Mode, OutputFormat,
    validate_credential_home_path,
};
use crate::normalize::{RunError, TurnOutput, normalize_output};
use crate::schema::Schema;

/// Cross-engine credentials that must NEVER leak into the `antigravity` child,
/// whether running in a sandbox or on the host.
pub const CROSS_ENGINE_STRIPPED_ENV: &[&str] = &["CLAUDE_CODE_OAUTH_TOKEN", "ANTHROPIC_API_KEY"];

/// Environment variables that must NEVER leak into a sandboxed `antigravity`
/// child (`host_mode = false`): GitHub tokens and other engines' credentials.
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
    /// Optional explicit `$HOME` for the child process (defaults to the
    /// inherited `HOME` environment variable).
    pub home_dir: Option<PathBuf>,
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
            dangerously_skip_permissions: config.dangerously_skip_permissions && !config.host_mode,
            sandbox: config.sandbox,
            disable_slash_commands: config.disable_slash_commands,
            settings_file: config.settings_file.clone(),
            output_format: config.output_format,
            input_format: config.input_format,
            print_timeout_secs: config.print_timeout_secs,
            home_dir: None,
        }
    }
}

/// Whether this invocation delivers its prompt as a `stream-json` NDJSON user
/// message on stdin (avoiding `ARG_MAX` / `MAX_ARG_STRLEN` limits and cmdline
/// exposure) rather than via `--print=<prompt>`.
///
/// Real `antigravity` supports `--input-format stream-json` whenever
/// `--output-format stream-json` is active (the default for all burrow turns),
/// and requires `--print=<prompt>` for single-shot `json`/`text` output modes.
fn uses_stream_json_stdin(invocation: &Invocation) -> bool {
    invocation.output_format == OutputFormat::StreamJson
        && invocation.input_format != Some(InputFormat::Text)
}

/// Render the verified headless `antigravity` CLI argument vector.
pub fn build_args(invocation: &Invocation) -> Vec<OsString> {
    let stream_stdin = uses_stream_json_stdin(invocation);
    let mut args: Vec<OsString> = vec![
        OsString::from("--output-format"),
        OsString::from(invocation.output_format.as_str()),
    ];
    if stream_stdin {
        args.push(OsString::from("--input-format"));
        args.push(OsString::from(InputFormat::StreamJson.as_str()));
    } else if let Some(input_format) = invocation.input_format {
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
    // Real `antigravity` parses `-p` / `--print` as a flag taking a string
    // value: in `stream-json` input mode pass `--print=` and stream the prompt
    // as NDJSON on stdin; in single-shot `json`/`text` mode attach the prompt
    // directly with `--print=<prompt>` so leading `-` is never parsed as a flag.
    if stream_stdin {
        args.push(OsString::from("--print="));
    } else {
        args.push(OsString::from(format!("--print={}", invocation.prompt)));
    }
    args
}

struct PreparedCredential {
    env_var: Option<(String, OsString)>,
    secrets: Vec<String>,
    _tempdir: Option<TempDir>,
    cleanup_home_file: Option<(PathBuf, PathBuf)>,
}

impl Drop for PreparedCredential {
    fn drop(&mut self) {
        if let Some((home, relative_path)) = &self.cleanup_home_file {
            remove_materialized_home_file(home, relative_path);
        }
    }
}

enum IsolatedHostHome {
    Temp(TempDir),
    Persistent(PathBuf),
}

impl IsolatedHostHome {
    fn path(&self) -> &Path {
        match self {
            Self::Temp(dir) => dir.path(),
            Self::Persistent(dir) => dir.as_path(),
        }
    }

    fn is_persistent(&self) -> bool {
        matches!(self, Self::Persistent(_))
    }
}

fn prepare_persistent_host_home(path: &Path, orig_home: Option<&Path>) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err(format!(
            "{HOST_HOME_ENV} must be an absolute path: {}",
            path.display()
        ));
    }
    if let Some(real_home) = orig_home
        && path == real_home
    {
        return Err(format!(
            "{HOST_HOME_ENV} must not match HOME ({})",
            real_home.display()
        ));
    }
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(format!(
                    "refusing to follow symlink at {HOST_HOME_ENV} {}",
                    path.display()
                ));
            }
            if !meta.is_dir() {
                return Err(format!(
                    "{HOST_HOME_ENV} {} is not a directory",
                    path.display()
                ));
            }
            if let Some(real_home) = orig_home
                && let (Ok(canon_path), Ok(canon_home)) =
                    (fs::canonicalize(path), fs::canonicalize(real_home))
                && canon_path == canon_home
            {
                return Err(format!(
                    "{HOST_HOME_ENV} must not resolve to HOME ({})",
                    real_home.display()
                ));
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            DirBuilder::new().mode(0o700).create(path).map_err(|e| {
                format!(
                    "failed to create persistent host HOME {}: {e}",
                    path.display()
                )
            })?;
        }
        Err(e) => {
            return Err(format!(
                "failed to inspect persistent host HOME {}: {e}",
                path.display()
            ));
        }
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|e| {
        format!(
            "failed to secure persistent host HOME {}: {e}",
            path.display()
        )
    })?;
    Ok(path.to_path_buf())
}

fn remove_materialized_home_file(home: &Path, relative_path: &Path) {
    if validate_credential_home_path(relative_path).is_err() {
        return;
    }
    let Ok(home_meta) = fs::symlink_metadata(home) else {
        return;
    };
    if home_meta.file_type().is_symlink() || !home_meta.is_dir() {
        return;
    }
    let mut dir = home.to_path_buf();
    if let Some(parent) = relative_path.parent() {
        for comp in parent.components() {
            let Component::Normal(part) = comp else {
                return;
            };
            dir.push(part);
            let Ok(meta) = fs::symlink_metadata(&dir) else {
                return;
            };
            if meta.file_type().is_symlink() || !meta.is_dir() {
                return;
            }
        }
    }
    let Some(file_name) = relative_path.file_name() else {
        return;
    };
    let target = dir.join(file_name);
    if fs::symlink_metadata(&target).is_ok_and(|meta| meta.file_type().is_file()) {
        let _ = fs::remove_file(&target);
    }
}

fn collect_secrets(secret_value: Option<&str>) -> Vec<String> {
    let mut secrets = Vec::new();
    let Some(raw) = secret_value else {
        return secrets;
    };
    secrets.push(raw.to_owned());
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) {
        collect_json_secrets(&value, &mut secrets);
    }
    secrets
}

fn collect_json_secrets(value: &serde_json::Value, out: &mut Vec<String>) {
    const SECRET_KEYS: &[&str] = &[
        "access_token",
        "refresh_token",
        "id_token",
        "token",
        "api_key",
        "client_secret",
        "accessToken",
        "refreshToken",
        "idToken",
        "apiKey",
        "clientSecret",
    ];
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                if SECRET_KEYS.contains(&k.as_str())
                    && let Some(s) = v.as_str()
                    && !s.trim().is_empty()
                    && !out.iter().any(|existing| existing == s)
                {
                    out.push(s.to_owned());
                } else {
                    collect_json_secrets(v, out);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_json_secrets(item, out);
            }
        }
        _ => {}
    }
}

fn materialize_home_file(
    home: &Path,
    relative_path: &Path,
    value: &str,
) -> Result<PathBuf, String> {
    validate_credential_home_path(relative_path)?;
    if home.as_os_str().is_empty() {
        return Err("credential_mode \"home-file\" requires HOME to be set".to_owned());
    }
    let home_meta = fs::metadata(home)
        .map_err(|e| format!("failed to access HOME directory {}: {e}", home.display()))?;
    if !home_meta.is_dir() {
        return Err(format!("HOME {} is not a directory", home.display()));
    }

    let mut dir = home.to_path_buf();
    if let Some(parent) = relative_path.parent() {
        for comp in parent.components() {
            let Component::Normal(part) = comp else {
                return Err(format!(
                    "credential_home_path {:?} must contain only normal relative path components",
                    relative_path
                ));
            };
            dir.push(part);
            match fs::symlink_metadata(&dir) {
                Ok(meta) => {
                    if meta.file_type().is_symlink() {
                        return Err(format!(
                            "refusing to follow symlink at credential directory {}",
                            dir.display()
                        ));
                    }
                    if !meta.is_dir() {
                        return Err(format!(
                            "credential parent path {} is not a directory",
                            dir.display()
                        ));
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    DirBuilder::new().mode(0o700).create(&dir).map_err(|e| {
                        format!(
                            "failed to create isolated credential dir {}: {e}",
                            dir.display()
                        )
                    })?;
                }
                Err(e) => {
                    return Err(format!(
                        "failed to inspect isolated credential dir {}: {e}",
                        dir.display()
                    ));
                }
            }
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).map_err(|e| {
                format!(
                    "failed to secure isolated credential dir {}: {e}",
                    dir.display()
                )
            })?;
        }
    }

    let file_name = relative_path
        .file_name()
        .ok_or_else(|| "credential_home_path must name a file".to_owned())?;
    let target = dir.join(file_name);
    match fs::symlink_metadata(&target) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(format!(
                "refusing to write credential through symlink {}",
                target.display()
            ));
        }
        Ok(meta) if meta.is_dir() => {
            return Err(format!(
                "credential target path {} is a directory",
                target.display()
            ));
        }
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(format!(
                "failed to inspect isolated credential file {}: {e}",
                target.display()
            ));
        }
    }

    let mut tmp = tempfile::Builder::new()
        .prefix(".antigravity-oauth-token.")
        .tempfile_in(&dir)
        .map_err(|e| format!("failed to create temporary isolated credential file: {e}"))?;
    tmp.as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("failed to secure isolated credential file: {e}"))?;
    tmp.write_all(value.as_bytes())
        .map_err(|e| format!("failed to write isolated credential file: {e}"))?;
    tmp.as_file()
        .sync_all()
        .map_err(|e| format!("failed to flush isolated credential file: {e}"))?;
    tmp.persist(&target)
        .map_err(|e| format!("failed to persist isolated credential file: {}", e.error))?;
    Ok(target)
}

fn prepare_credential(
    config: &Config,
    home: Option<&Path>,
    cleanup_home_file: bool,
) -> Result<PreparedCredential, String> {
    let target_name = config
        .target_credential_env
        .as_deref()
        .or(config.credential_env.as_deref());

    let raw_from_file = match &config.credential_file {
        Some(path) => match fs::read_to_string(path) {
            Ok(text) => {
                Some(text.trim_end_matches(['\r', '\n']).to_owned()).filter(|s| !s.is_empty())
            }
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
    let secrets = collect_secrets(secret_value.as_deref());

    match config.credential_mode {
        CredentialMode::Env => {
            let env_var = match (target_name, &secret_value) {
                (Some(name), Some(val)) => Some((name.to_owned(), OsString::from(val))),
                _ => None,
            };
            Ok(PreparedCredential {
                env_var,
                secrets,
                _tempdir: None,
                cleanup_home_file: None,
            })
        }
        CredentialMode::File => {
            let Some(name) = target_name else {
                return Ok(PreparedCredential {
                    env_var: None,
                    secrets,
                    _tempdir: None,
                    cleanup_home_file: None,
                });
            };
            if let Some(path) = &config.credential_file {
                return Ok(PreparedCredential {
                    env_var: Some((name.to_owned(), path.as_os_str().to_os_string())),
                    secrets,
                    _tempdir: None,
                    cleanup_home_file: None,
                });
            }
            let Some(val) = &secret_value else {
                return Ok(PreparedCredential {
                    env_var: None,
                    secrets,
                    _tempdir: None,
                    cleanup_home_file: None,
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
                secrets,
                _tempdir: Some(dir),
                cleanup_home_file: None,
            })
        }
        CredentialMode::HomeFile => {
            validate_credential_home_path(&config.credential_home_path)?;
            let Some(val) = &secret_value else {
                return Ok(PreparedCredential {
                    env_var: None,
                    secrets,
                    _tempdir: None,
                    cleanup_home_file: None,
                });
            };
            let home = home.ok_or_else(|| {
                "credential_mode \"home-file\" requires HOME to be set".to_owned()
            })?;
            materialize_home_file(home, &config.credential_home_path, val)?;
            Ok(PreparedCredential {
                env_var: None,
                secrets,
                _tempdir: None,
                cleanup_home_file: cleanup_home_file
                    .then(|| (home.to_path_buf(), config.credential_home_path.clone())),
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
    let orig_home = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from);
    // In host mode, always run in a private `$HOME` (regardless of credential
    // mode) so interactive desktop or T3 userdata in the real `$HOME` is never
    // read or overwritten. When `PROJ_ENGINE_HOME` is provided by `burrow-proj`,
    // persist that per-job directory across turns so `--conversation` resumes
    // find prior Antigravity session state; otherwise use a per-turn tempdir.
    let isolated_host_home = if config.host_mode && invocation.home_dir.is_none() {
        let persistent = std::env::var_os(HOST_HOME_ENV)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from);
        if let Some(dir) = persistent {
            match prepare_persistent_host_home(&dir, orig_home.as_deref()) {
                Ok(dir) => Some(IsolatedHostHome::Persistent(dir)),
                Err(message) => {
                    return TurnOutput {
                        error: Some(RunError::ProcessFailed {
                            exit_code: None,
                            signal: None,
                            message,
                        }),
                        ..TurnOutput::default()
                    };
                }
            }
        } else {
            match tempfile::Builder::new()
                .prefix("antigravity-host-home-")
                .tempdir()
                .and_then(|dir| {
                    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700))?;
                    Ok(dir)
                }) {
                Ok(dir) => Some(IsolatedHostHome::Temp(dir)),
                Err(e) => {
                    return TurnOutput {
                        error: Some(RunError::ProcessFailed {
                            exit_code: None,
                            signal: None,
                            message: format!("failed to create isolated host HOME: {e}"),
                        }),
                        ..TurnOutput::default()
                    };
                }
            }
        }
    } else {
        None
    };
    let effective_home = invocation
        .home_dir
        .clone()
        .or_else(|| isolated_host_home.as_ref().map(|d| d.path().to_path_buf()));
    let resolved_home = effective_home.clone().or_else(|| orig_home.clone());
    let cleanup_home_file = isolated_host_home
        .as_ref()
        .is_some_and(IsolatedHostHome::is_persistent);
    let prepared = match prepare_credential(config, resolved_home.as_deref(), cleanup_home_file) {
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

    if let Some(home) = &effective_home {
        command.env("HOME", home);
        if isolated_host_home.is_some()
            && let Some(real_home) = &orig_home
        {
            if std::env::var_os("GIT_CONFIG_GLOBAL").is_none() {
                let gitconfig = real_home.join(".gitconfig");
                if gitconfig.is_file() {
                    command.env("GIT_CONFIG_GLOBAL", gitconfig);
                }
            }
            if std::env::var_os("CARGO_HOME").is_none() {
                let cargo_home = real_home.join(".cargo");
                if cargo_home.is_dir() {
                    command.env("CARGO_HOME", cargo_home);
                }
            }
        }
    }

    let stripped = if config.host_mode {
        CROSS_ENGINE_STRIPPED_ENV
    } else {
        STRIPPED_ENV
    };
    for var in stripped {
        command.env_remove(var);
    }
    command.env_remove(HOST_HOME_ENV);
    match config.credential_mode {
        CredentialMode::HomeFile => {
            if let Some(src_env) = &config.credential_env {
                command.env_remove(src_env);
            }
            if let Some(target_env) = &config.target_credential_env {
                command.env_remove(target_env);
            }
        }
        CredentialMode::Env | CredentialMode::File => {
            if let Some(src_env) = &config.credential_env
                && config.target_credential_env.as_deref() != Some(src_env.as_str())
            {
                command.env_remove(src_env);
            }
        }
    }
    if let Some((key, val)) = &prepared.env_var {
        command.env(key, val);
    }

    let deadline = Instant::now() + Duration::from_secs(config.deadline_secs);
    // In `stream-json` input mode, deliver the prompt as a single NDJSON
    // `{"event":"user","message":{"content":...}}` line on stdin (which
    // `output_unless` closes immediately on write, and binds to `/dev/null`
    // when empty). In single-shot `json`/`text` mode, `--print=<prompt>` carries
    // the prompt and stdin stays `/dev/null`.
    let stdin_payload: Option<Vec<u8>> =
        if uses_stream_json_stdin(invocation) && !invocation.prompt.is_empty() {
            let mut line = serde_json::to_vec(&serde_json::json!({
                "event": "user",
                "message": { "content": invocation.prompt },
            }))
            .expect("stream-json user prompt serialization cannot fail");
            line.push(b'\n');
            Some(line)
        } else {
            None
        };
    let result = burrow_process::output_unless(
        &mut command,
        stdin_payload.as_deref(),
        deadline,
        config.max_output_bytes,
        stop,
    );

    let secrets: Vec<&str> = prepared.secrets.iter().map(String::as_str).collect();

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
