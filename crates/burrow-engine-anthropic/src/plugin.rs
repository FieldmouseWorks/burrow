//! `conary-plugin v5` `job` executable for Anthropic's `claude` subscription-CLI
//! and `claude-api` credit modes.
//!
//! Reads one JSON [`Request`] on stdin and writes one JSON [`Response`] on
//! stdout. Usage, refusals, and failures are reported as structured JSON lines
//! on stderr (never on stdout, and never containing credentials).

pub mod grants;
pub mod run;

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use conary_core::plugin::{
    Answer, CONTRACTS, Capabilities, Context, Executed, Execution, Identity, Kind, Model, Offers,
    Request, Response, Sandbox, Task, fixtures, version,
};
use serde::{Deserialize, Serialize};

use crate::Mode;

/// Environment variable naming the JSON runtime configuration file.
pub const CONFIG_ENV: &str = "BURROW_ANTHROPIC_CONFIG";

/// Environment variable selecting the mode (`claude` or `claude-api`) when no
/// config file is supplied.
pub const MODE_ENV: &str = "BURROW_ANTHROPIC_MODE";

/// Process exit codes: `0` on success, nonzero on any failure or refusal so
/// Conary's runner treats refusals and failures as unsuccessful invocations.
pub const EXIT_OK: u8 = 0;
pub const EXIT_FAILURE: u8 = 1;
pub const EXIT_REFUSED_USAGE: u8 = 2;
pub const EXIT_REFUSED_AUTH: u8 = 3;

/// Session identifier reported on Conary's offline conformance fixture.
pub const CONFORMANCE_SESSION: &str = "burrow-engine-anthropic-conformance";

/// Maximum request size accepted on stdin (matches `burrow-agent`'s cap).
const MAX_REQUEST_BYTES: u64 = 32 * 1024 * 1024;

fn default_true() -> bool {
    true
}

/// Optional capability overrides in [`PluginConfig`]. `sandbox` is always
/// [`Sandbox::Needs`] because this plugin runs in burrow's shared sandbox.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeclaredCapabilities {
    #[serde(default = "default_true")]
    pub network: bool,
    #[serde(default = "default_true")]
    pub mcp: bool,
    #[serde(default)]
    pub devices: Vec<String>,
}

impl Default for DeclaredCapabilities {
    fn default() -> Self {
        Self {
            network: true,
            mcp: true,
            devices: Vec::new(),
        }
    }
}

/// Optional credit admission settings for [`Mode::ClaudeApi`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreditConfig {
    pub credit_file: PathBuf,
    pub jobs_dir: PathBuf,
}

/// Runtime configuration supplied by the machine / Nix surface (never names
/// model IDs in tracked files).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginConfig {
    #[serde(default)]
    pub mode: Mode,
    #[serde(default)]
    pub models: Vec<Model>,
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default)]
    pub default_effort: Option<String>,
    #[serde(default)]
    pub capabilities: Option<DeclaredCapabilities>,
    #[serde(default)]
    pub meters: Option<Vec<String>>,
    #[serde(default)]
    pub program: Option<PathBuf>,
    #[serde(default)]
    pub credential_env: Option<String>,
    #[serde(default)]
    pub api_key_file: Option<PathBuf>,
    #[serde(default)]
    pub api_base_url: Option<String>,
    #[serde(default)]
    pub max_budget_usd: Option<i64>,
    #[serde(default)]
    pub credit: Option<CreditConfig>,
    #[serde(default)]
    pub append_system_prompt: Option<String>,
    /// True only for host-mode `proj` wrappers (never inside sandboxes):
    /// allows host `~/.claude` login, host environment inheritance, and live
    /// host capability/system-prompt handoff.
    #[serde(default)]
    pub host_mode: bool,
}

impl PluginConfig {
    /// Load configuration from a JSON file on disk.
    pub fn from_file(path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let cfg: Self =
            serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Load configuration from `BURROW_ANTHROPIC_CONFIG`, or fall back to
    /// defaults (respecting `BURROW_ANTHROPIC_MODE` if set).
    pub fn from_env() -> Result<Self, String> {
        if let Some(path) = std::env::var_os(CONFIG_ENV).filter(|v| !v.is_empty()) {
            return Self::from_file(Path::new(&path));
        }
        let mode = match std::env::var(MODE_ENV).ok().as_deref() {
            None | Some("") | Some("claude") => Mode::Claude,
            Some("claude-api") => Mode::ClaudeApi,
            Some(other) => {
                return Err(format!(
                    "{MODE_ENV}: expected \"claude\" or \"claude-api\", got {other:?}"
                ));
            }
        };
        let cfg = Self {
            mode,
            ..Self::default()
        };
        cfg.validate()?;
        Ok(cfg)
    }

    /// Validate offers, defaults, and budget configuration.
    pub fn validate(&self) -> Result<(), String> {
        let offers = self.offers();
        if !offers.valid(&[Kind::Job]) {
            return Err("invalid configured models, capabilities, or meters".to_owned());
        }
        if let Some(model) = &self.default_model
            && !offers.allows(Some(model.as_str()), None)
        {
            return Err(format!(
                "defaultModel {model:?} is not in the configured models"
            ));
        }
        if let Some(effort) = &self.default_effort {
            let effective_model = self
                .default_model
                .as_deref()
                .or_else(|| self.models.first().map(|m| m.id.as_str()));
            if !offers.allows(effective_model, Some(effort.as_str())) {
                return Err(format!(
                    "defaultEffort {effort:?} is not offered by the default model"
                ));
            }
        }
        if let Some(budget) = self.max_budget_usd
            && budget <= 0
        {
            return Err("maxBudgetUsd must be positive".to_owned());
        }
        Ok(())
    }

    pub fn engine_name(&self) -> &'static str {
        self.mode.name()
    }

    pub fn program(&self) -> &Path {
        self.program
            .as_deref()
            .unwrap_or_else(|| Path::new(crate::command::PROGRAM))
    }

    pub fn credential_env(&self) -> &str {
        self.credential_env
            .as_deref()
            .unwrap_or_else(|| self.mode.default_credential_env())
    }

    /// Capabilities declared by `describe`: always [`Sandbox::Needs`].
    pub fn capabilities(&self) -> Capabilities {
        let declared = self.capabilities.clone().unwrap_or_default();
        Capabilities {
            sandbox: Sandbox::Needs,
            network: declared.network,
            devices: declared.devices,
            mcp: declared.mcp,
        }
    }

    /// Usage meters declared by `describe`.
    pub fn meters(&self) -> Vec<String> {
        if let Some(meters) = &self.meters {
            return meters.clone();
        }
        match self.mode {
            Mode::Claude => vec!["five_hour".to_owned(), "seven_day".to_owned()],
            Mode::ClaudeApi => vec!["credit".to_owned()],
        }
    }

    /// The [`Offers`] advertised by `describe`.
    pub fn offers(&self) -> Offers {
        Offers {
            models: self.models.clone(),
            capabilities: self.capabilities(),
            meters: self.meters(),
        }
    }
}

/// Normalized token and cost usage extracted from a Claude stream run.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct JobUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_cost_usd: Option<f64>,
}

/// Structured refusal category classified inside `burrow-engine-anthropic`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalKind {
    UsageLimit,
    LoginExpired,
    ApiKeyRejected,
    BudgetExhausted,
    CreditExhausted,
}

impl RefusalKind {
    pub fn exit_code(self) -> u8 {
        match self {
            Self::UsageLimit | Self::BudgetExhausted | Self::CreditExhausted => EXIT_REFUSED_USAGE,
            Self::LoginExpired | Self::ApiKeyRejected => EXIT_REFUSED_AUTH,
        }
    }
}

/// A classified engine refusal (never leaks credentials or billing URLs).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refusal {
    pub kind: RefusalKind,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<i64>,
}

/// Structured outcome of a real `job` invocation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum JobOutcome {
    Ok {
        session: Option<String>,
        answer: String,
        usage: JobUsage,
    },
    Refused {
        session: Option<String>,
        refusal: Refusal,
        usage: JobUsage,
    },
    Failed {
        session: Option<String>,
        error: String,
        usage: JobUsage,
    },
}

/// Structured event logged to stderr for observability and future callers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum DiagnosticEvent {
    Usage {
        engine: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        usage: JobUsage,
    },
    Refusal {
        engine: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        refusal: Refusal,
        usage: JobUsage,
    },
    Error {
        engine: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        error: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<JobUsage>,
    },
    /// Optional real-time raw stream line forwarded when `PROJ_STREAM_EVENTS=1`.
    Stream { line: String },
}

/// Final outcome of handling one `conary-plugin v5` request.
#[derive(Debug)]
pub struct PluginResult {
    pub response: Option<Response>,
    pub diagnostic: Option<DiagnosticEvent>,
    pub exit_code: u8,
}

impl PluginResult {
    fn ok(response: Response, diagnostic: Option<DiagnosticEvent>) -> Self {
        Self {
            response: Some(response),
            diagnostic,
            exit_code: EXIT_OK,
        }
    }

    fn error(
        engine: &str,
        session: Option<String>,
        error: String,
        usage: Option<JobUsage>,
    ) -> Self {
        Self {
            response: None,
            diagnostic: Some(DiagnosticEvent::Error {
                engine: engine.to_owned(),
                session,
                error,
                usage,
            }),
            exit_code: EXIT_FAILURE,
        }
    }

    fn refused(engine: &str, session: Option<String>, refusal: Refusal, usage: JobUsage) -> Self {
        let exit_code = refusal.kind.exit_code();
        Self {
            response: None,
            diagnostic: Some(DiagnosticEvent::Refusal {
                engine: engine.to_owned(),
                session,
                refusal,
                usage,
            }),
            exit_code,
        }
    }
}

/// Build the `describe` answer for `config`.
pub fn describe(config: &PluginConfig) -> Answer {
    let offers = config.offers();
    Answer::Describe {
        versions: CONTRACTS.map(str::to_owned).to_vec(),
        kinds: Vec::new(),
        v2_kinds: vec![Kind::Job],
        v4_kinds: Vec::new(),
        models: offers.models,
        capabilities: Some(offers.capabilities),
        meters: offers.meters,
    }
}

/// Whether `task` is Conary's reserved offline conformance job.
pub fn is_conformance_job(task: &Task) -> bool {
    let Task::Job {
        repo,
        workdir,
        prompt,
        ..
    } = task
    else {
        return false;
    };
    fixtures().into_iter().any(|f| {
        matches!(
            f.task,
            Task::Job {
                repo: r,
                workdir: w,
                prompt: p,
                ..
            } if r == *repo && w == *workdir && p == *prompt
        )
    })
}

/// Resolve the effective `(model, effort)` for a job request against `offers`.
fn resolve_choice(
    config: &PluginConfig,
    offers: &Offers,
    task_model: Option<&str>,
    task_effort: Option<&str>,
    execution: Option<&Execution>,
) -> Result<(Option<String>, Option<String>), String> {
    let choice = execution.and_then(|e| e.choice.as_ref());
    if let Some(c) = choice {
        if c.engine != config.engine_name() {
            return Err(format!(
                "execution choice names engine {:?}, not {:?}",
                c.engine,
                config.engine_name()
            ));
        }
        if task_model.is_some_and(|m| c.model.as_deref() != Some(m))
            || task_effort.is_some_and(|e| c.effort.as_deref() != Some(e))
        {
            return Err("job and execution choices disagree".to_owned());
        }
    }

    let requested_model = choice
        .and_then(|c| c.model.as_deref())
        .or(task_model)
        .map(str::to_owned);
    let requested_effort = choice
        .and_then(|c| c.effort.as_deref())
        .or(task_effort)
        .map(str::to_owned);

    if requested_model.is_none() && requested_effort.is_some() {
        return Err("effort cannot be selected without a model".to_owned());
    }
    if requested_model.is_some()
        && !offers.allows(requested_model.as_deref(), requested_effort.as_deref())
    {
        return Err("requested model or effort is not offered".to_owned());
    }

    let model = match requested_model {
        Some(m) => Some(m),
        None if !offers.models.is_empty() => config
            .default_model
            .as_deref()
            .filter(|m| offers.allows(Some(m), None))
            .map(str::to_owned)
            .or_else(|| offers.models.first().map(|m| m.id.clone())),
        None => None,
    };

    let effort = match requested_effort {
        Some(e) => Some(e),
        None => match (&model, &config.default_effort) {
            (Some(m), Some(e)) if offers.allows(Some(m.as_str()), Some(e.as_str())) => {
                Some(e.clone())
            }
            _ => None,
        },
    };

    if !offers.allows(model.as_deref(), effort.as_deref()) {
        return Err("resolved model or effort is not offered".to_owned());
    }
    Ok((model, effort))
}

fn verify_v5(request: &Request, response: &Response) -> Result<(), String> {
    if let Some(negotiated) = &request.negotiated {
        let execution = request
            .execution
            .as_ref()
            .ok_or_else(|| "missing execution on v5 request".to_owned())?;
        let provenance = response
            .provenance()
            .ok_or_else(|| "missing provenance on v5 response".to_owned())?;
        negotiated
            .verify(execution, &provenance)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Handle one decoded `conary-plugin v5` [`Request`].
pub fn handle_request(
    config: &PluginConfig,
    request: &Request,
    env: &dyn Fn(&str) -> Option<String>,
    now: i64,
) -> PluginResult {
    handle_request_with_stream(config, request, env, now, &mut |_| {})
}

/// [`handle_request`], invoking `on_stream` for each stdout line emitted while
/// the harness runs.
pub fn handle_request_with_stream(
    config: &PluginConfig,
    request: &Request,
    env: &dyn Fn(&str) -> Option<String>,
    now: i64,
    on_stream: &mut dyn FnMut(&str),
) -> PluginResult {
    let engine = config.engine_name();
    if version(&request.contract).is_none() {
        return PluginResult::error(
            engine,
            None,
            format!("unsupported contract {:?}", request.contract),
            None,
        );
    }
    if let Err(e) = config.validate() {
        return PluginResult::error(engine, None, e, None);
    }

    match &request.task {
        Task::Describe => PluginResult::ok(
            Response {
                contract: request.contract.clone(),
                by: Identity {
                    engine: engine.to_owned(),
                    session: None,
                },
                execution: None,
                answer: describe(config),
            },
            None,
        ),
        Task::Job {
            repo,
            workdir,
            prompt,
            model,
            effort,
        } => {
            let offers = request
                .negotiated
                .as_ref()
                .map(|n| &n.offers)
                .cloned()
                .unwrap_or_else(|| config.offers());
            let (chosen_model, chosen_effort) = match resolve_choice(
                config,
                &offers,
                model.as_deref(),
                effort.as_deref(),
                request.execution.as_ref(),
            ) {
                Ok(pair) => pair,
                Err(why) => return PluginResult::error(engine, None, why, None),
            };

            if is_conformance_job(&request.task) {
                let context = request
                    .execution
                    .as_ref()
                    .map(|e| e.context.clone())
                    .unwrap_or_default();
                let session = match &context {
                    Context::Continue { session } => session.clone(),
                    Context::Fresh | Context::Fork { .. } => CONFORMANCE_SESSION.to_owned(),
                };
                let response = Response {
                    contract: request.contract.clone(),
                    by: Identity {
                        engine: engine.to_owned(),
                        session: Some(session),
                    },
                    execution: request.execution.as_ref().map(|e| Executed {
                        model: chosen_model,
                        effort: chosen_effort,
                        context: e.context.clone(),
                    }),
                    answer: Answer::Job {
                        summary: "conformance fixture completed without invoking Anthropic"
                            .to_owned(),
                        next: conary_core::plugin::Next::Done,
                    },
                };
                return match verify_v5(request, &response) {
                    Ok(()) => PluginResult::ok(response, None),
                    Err(why) => PluginResult::error(engine, None, why, None),
                };
            }

            if repo.trim().is_empty()
                || workdir.trim().is_empty()
                || workdir.contains('\0')
                || prompt.trim().is_empty()
            {
                return PluginResult::error(
                    engine,
                    None,
                    "a job needs a nonblank repo, workdir, and prompt".to_owned(),
                    None,
                );
            }

            let context = request
                .execution
                .as_ref()
                .map(|e| e.context.clone())
                .unwrap_or_default();
            if let Context::Continue { session } | Context::Fork { session } = &context
                && session.trim().is_empty()
            {
                return PluginResult::error(
                    engine,
                    None,
                    "continuation or fork requires a nonblank session".to_owned(),
                    None,
                );
            }

            let grants = match grants::from_env_with_host(env, Path::new(workdir), config.host_mode)
            {
                Ok(g) => g,
                Err(why) => return PluginResult::error(engine, None, why, None),
            };

            // A `claude-api` run's budget, as its settings cap it; a metered
            // credit's admission caps it again, just before the run starts.
            let budget = match config.mode {
                Mode::ClaudeApi => config.max_budget_usd,
                Mode::Claude => None,
            };

            let spec = run::JobSpec {
                workdir: Path::new(workdir),
                prompt,
                model: chosen_model.as_deref(),
                effort: chosen_effort.as_deref(),
                context: &context,
                max_budget_usd: budget,
                credit: config.credit.as_ref(),
            };

            match run::execute_with_stream(config, &grants, &spec, env, now, on_stream) {
                JobOutcome::Ok {
                    session,
                    answer,
                    usage,
                } => {
                    let response = Response {
                        contract: request.contract.clone(),
                        by: Identity {
                            engine: engine.to_owned(),
                            session: session.clone(),
                        },
                        execution: request.execution.as_ref().map(|e| Executed {
                            model: chosen_model,
                            effort: chosen_effort,
                            context: e.context.clone(),
                        }),
                        answer: Answer::Job {
                            summary: answer,
                            next: conary_core::plugin::Next::Done,
                        },
                    };
                    match verify_v5(request, &response) {
                        Ok(()) => PluginResult::ok(
                            response,
                            Some(DiagnosticEvent::Usage {
                                engine: engine.to_owned(),
                                session,
                                usage,
                            }),
                        ),
                        Err(why) => PluginResult::error(engine, session, why, Some(usage)),
                    }
                }
                JobOutcome::Refused {
                    session,
                    refusal,
                    usage,
                } => PluginResult::refused(engine, session, refusal, usage),
                JobOutcome::Failed {
                    session,
                    error,
                    usage,
                } => PluginResult::error(engine, session, error, Some(usage)),
            }
        }
        _ => PluginResult::error(
            engine,
            None,
            "this plugin serves describe and job alone".to_owned(),
            None,
        ),
    }
}

/// Read one JSON [`Request`] from `input`, run it, write stdout/stderr, and
/// return the process exit code.
pub fn run_stdio(
    config: &PluginConfig,
    input: &mut dyn Read,
    output: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    let mut bytes = Vec::new();
    if let Err(e) = input.take(MAX_REQUEST_BYTES + 1).read_to_end(&mut bytes) {
        let _ = writeln!(err, "burrow-engine-anthropic: failed to read request: {e}");
        return EXIT_FAILURE;
    }
    if bytes.len() as u64 > MAX_REQUEST_BYTES {
        let _ = writeln!(err, "burrow-engine-anthropic: request exceeds size cap");
        return EXIT_FAILURE;
    }
    let request: Request = match serde_json::from_slice(&bytes) {
        Ok(r) => r,
        Err(e) => {
            let _ = writeln!(err, "burrow-engine-anthropic: invalid request JSON: {e}");
            return EXIT_FAILURE;
        }
    };
    let now = jiff::Timestamp::now().as_second();
    let stream_events = std::env::var("PROJ_STREAM_EVENTS")
        .ok()
        .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
    let result = handle_request_with_stream(
        config,
        &request,
        &|name| std::env::var(name).ok(),
        now,
        &mut |line| {
            if stream_events
                && let Ok(encoded) = serde_json::to_string(&DiagnosticEvent::Stream {
                    line: line.to_owned(),
                })
            {
                let _ = writeln!(err, "{encoded}");
                let _ = err.flush();
            }
        },
    );
    if let Some(diag) = &result.diagnostic
        && let Ok(line) = serde_json::to_string(diag)
    {
        let _ = writeln!(err, "{line}");
    }
    if let Some(response) = &result.response {
        let written = serde_json::to_writer(&mut *output, response)
            .map_err(|e| e.to_string())
            .and_then(|()| output.write_all(b"\n").map_err(|e| e.to_string()))
            .and_then(|()| output.flush().map_err(|e| e.to_string()));
        if let Err(e) = written {
            let _ = writeln!(
                err,
                "burrow-engine-anthropic: failed to write response: {e}"
            );
            return EXIT_FAILURE;
        }
    }
    result.exit_code
}

#[cfg(test)]
mod tests;
