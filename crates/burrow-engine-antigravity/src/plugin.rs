//! Conary `conary-plugin v5` (`describe` + `work` for `job`) handler.

use std::path::PathBuf;

use conary_core::plugin::{
    Answer, CONTRACT_V1, CONTRACTS, Context, Executed, Identity, Kind, Next, Offers, Request,
    Response, Task, fixtures, version,
};

use crate::cli::{Invocation, run_invocation};
use crate::config::{Config, ENGINE, Effort};
use crate::normalize::{Refusal, RunError, Usage};

/// Exit codes aligned with burrow's Conary plugin diagnostics contract.
pub const EXIT_OK: u8 = 0;
pub const EXIT_FAILURE: u8 = 1;
pub const EXIT_REFUSED_USAGE: u8 = 2;
pub const EXIT_REFUSED_AUTH: u8 = 3;

/// Structured outcome for stdio invocation: stdout response (when successful),
/// structured JSON diagnostic event for stderr, and exit code.
#[derive(Debug)]
pub struct PluginResult {
    pub response: Option<Response>,
    pub diagnostic: Option<serde_json::Value>,
    pub exit_code: u8,
}

/// Deterministic nonblank session reported for offline conformance fixtures.
const CONFORMANCE_SESSION: &str = "antigravity-conformance";

/// Build the offline `describe` response for `config`.
pub fn describe(config: &Config) -> Response {
    let offers = config.offers();
    Response {
        contract: CONTRACT_V1.to_owned(),
        by: Identity {
            engine: ENGINE.to_owned(),
            session: None,
        },
        execution: None,
        answer: Answer::Describe {
            versions: CONTRACTS.map(str::to_owned).to_vec(),
            kinds: Vec::new(),
            v2_kinds: vec![Kind::Job],
            v4_kinds: Vec::new(),
            models: offers.models,
            capabilities: Some(offers.capabilities),
            meters: offers.meters,
        },
    }
}

/// Whether `task` is Conary's reserved offline conformance job fixture.
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
        matches!(f.task, Task::Job { repo: r, workdir: w, prompt: p, .. }
            if r == *repo && w == *workdir && p == *prompt)
    })
}

/// What a runner tells the engine for the turn (burrow-proj's
/// `PROJ_APPEND_SYSTEM_PROMPT`: how to declare its next step, and where a
/// sandbox records it) follows the task in a job's prompt, since Antigravity
/// has no system prompt of its own. Conary's conformance fixture, matched on
/// its exact prompt, is left as it is.
pub fn add_instructions(request: &mut Request, instructions: Option<&str>) {
    let Some(extra) = instructions.map(str::trim).filter(|s| !s.is_empty()) else {
        return;
    };
    if is_conformance_job(&request.task) {
        return;
    }
    if let Task::Job { prompt, .. } = &mut request.task {
        prompt.push_str("\n\n");
        prompt.push_str(extra);
    }
}

/// Answer one Conary plugin request without external cancellation.
pub fn answer(config: &Config, request: &Request) -> Result<Response, String> {
    answer_cancellable(config, request, &|| false)
}

/// Answer one Conary plugin request and return structured diagnostics + exit code
/// for stdio callers (`burrow-proj`, `conaryd`).
pub fn handle_request(config: &Config, request: &Request, stop: &dyn Fn() -> bool) -> PluginResult {
    match answer_detailed(config, request, stop) {
        Ok((response, usage)) => {
            let diagnostic = usage.map(|u| usage_event(response.by.session.as_deref(), &u));
            PluginResult {
                response: Some(response),
                diagnostic,
                exit_code: EXIT_OK,
            }
        }
        Err(err) => match *err {
            DetailedError::Refusal {
                session,
                refusal,
                usage,
            } => {
                let reason = RunError::Refusal(refusal.clone()).to_string();
                let (kind, resets_at, exit_code) = match refusal {
                    Refusal::Quota { reset_at, .. } => {
                        ("usage_limit", reset_at, EXIT_REFUSED_USAGE)
                    }
                    Refusal::Login { .. } => ("login_expired", None, EXIT_REFUSED_AUTH),
                };
                PluginResult {
                    response: None,
                    diagnostic: Some(serde_json::json!({
                        "event": "refusal",
                        "engine": ENGINE,
                        "session": session,
                        "refusal": {
                            "kind": kind,
                            "reason": reason,
                            "resets_at": resets_at,
                        },
                        "usage": usage_json(usage.as_ref()),
                    })),
                    exit_code,
                }
            }
            DetailedError::Other {
                session,
                error,
                usage,
            } => PluginResult {
                response: None,
                diagnostic: Some(serde_json::json!({
                    "event": "error",
                    "engine": ENGINE,
                    "session": session,
                    "error": error,
                    "usage": usage.as_ref().map(|u| usage_json(Some(u))),
                })),
                exit_code: EXIT_FAILURE,
            },
        },
    }
}

fn usage_json(usage: Option<&Usage>) -> serde_json::Value {
    match usage {
        Some(u) => serde_json::json!({
            "input_tokens": u.input_tokens.unwrap_or(0),
            "output_tokens": u.output_tokens.unwrap_or(0),
            "cache_read_input_tokens": u.cache_read_tokens.unwrap_or(0),
            "cache_creation_input_tokens": u.cache_write_tokens.unwrap_or(0),
            "total_cost_usd": u.cost_usd,
        }),
        None => serde_json::json!({
            "input_tokens": 0,
            "output_tokens": 0,
            "cache_read_input_tokens": 0,
            "cache_creation_input_tokens": 0,
        }),
    }
}

fn usage_event(session: Option<&str>, usage: &Usage) -> serde_json::Value {
    serde_json::json!({
        "event": "usage",
        "engine": ENGINE,
        "session": session,
        "usage": usage_json(Some(usage)),
    })
}

enum DetailedError {
    Refusal {
        session: Option<String>,
        refusal: Refusal,
        usage: Option<Usage>,
    },
    Other {
        session: Option<String>,
        error: String,
        usage: Option<Usage>,
    },
}

impl DetailedError {
    fn other(error: String) -> Box<Self> {
        Box::new(Self::Other {
            session: None,
            error,
            usage: None,
        })
    }

    fn message(self) -> String {
        match self {
            Self::Refusal { refusal, .. } => RunError::Refusal(refusal).to_string(),
            Self::Other { error, .. } => error,
        }
    }
}

/// Answer one Conary plugin request, aborting and killing the child process
/// group as soon as `stop()` returns `true`.
pub fn answer_cancellable(
    config: &Config,
    request: &Request,
    stop: &dyn Fn() -> bool,
) -> Result<Response, String> {
    answer_detailed(config, request, stop)
        .map(|(response, _)| response)
        .map_err(|e| e.message())
}

fn answer_detailed(
    config: &Config,
    request: &Request,
    stop: &dyn Fn() -> bool,
) -> Result<(Response, Option<Usage>), Box<DetailedError>> {
    config.validate().map_err(DetailedError::other)?;

    match &request.task {
        Task::Describe => Ok((describe(config), None)),
        Task::Job {
            workdir,
            prompt,
            model,
            effort,
            ..
        } => {
            let ver = version(&request.contract)
                .ok_or_else(|| format!("unsupported plugin contract {:?}", request.contract))
                .map_err(DetailedError::other)?;
            if ver < 2 {
                return Err(DetailedError::other(
                    "the job kind requires conary-plugin v2 or newer".to_owned(),
                ));
            }

            let offers = match &request.negotiated {
                Some(neg) => {
                    if neg.by.engine != ENGINE {
                        return Err(DetailedError::other(format!(
                            "negotiated engine {:?} does not match {ENGINE:?}",
                            neg.by.engine
                        )));
                    }
                    neg.offers.clone()
                }
                None => config.offers(),
            };

            let (resolved_model, resolved_effort) = resolve_execution(
                config,
                &offers,
                request,
                model.as_deref(),
                effort.as_deref(),
            )
            .map_err(DetailedError::other)?;

            let context = request
                .execution
                .as_ref()
                .map(|e| e.context.clone())
                .unwrap_or(Context::Fresh);

            if is_conformance_job(&request.task) {
                let session = match &context {
                    Context::Continue { session } => session.clone(),
                    Context::Fresh => CONFORMANCE_SESSION.to_owned(),
                    Context::Fork { .. } => {
                        return Err(DetailedError::other(
                            "antigravity plugin does not support fork context".to_owned(),
                        ));
                    }
                };
                return Ok((
                    Response {
                        contract: request.contract.clone(),
                        by: Identity {
                            engine: ENGINE.to_owned(),
                            session: Some(session),
                        },
                        execution: (ver >= 3).then(|| Executed {
                            model: resolved_model,
                            effort: resolved_effort.map(|e| e.as_str().to_owned()),
                            context,
                        }),
                        answer: Answer::Job {
                            summary: "antigravity conformance fixture: finished".to_owned(),
                            next: Next::Done,
                        },
                    },
                    None,
                ));
            }

            let conversation = match &context {
                Context::Fresh => None,
                Context::Continue { session } => {
                    if session.trim().is_empty() {
                        return Err(DetailedError::other(
                            "continue context requires a nonblank session id".to_owned(),
                        ));
                    }
                    Some(session.clone())
                }
                Context::Fork { .. } => {
                    return Err(DetailedError::other(
                        "antigravity plugin does not support fork context".to_owned(),
                    ));
                }
            };

            let invocation = Invocation::from_config(
                config,
                PathBuf::from(workdir),
                prompt.clone(),
                resolved_model.clone(),
                resolved_effort,
                conversation.clone(),
            );
            let turn = run_invocation(config, &invocation, stop);

            if let Some(err) = turn.error {
                return match err {
                    RunError::Refusal(refusal) => Err(Box::new(DetailedError::Refusal {
                        session: turn.session,
                        refusal,
                        usage: turn.usage,
                    })),
                    other => Err(Box::new(DetailedError::Other {
                        session: turn.session,
                        error: other.to_string(),
                        usage: turn.usage,
                    })),
                };
            }

            let session = match (&conversation, turn.session) {
                (Some(expected), Some(actual)) if actual != *expected => {
                    return Err(Box::new(DetailedError::Other {
                        session: Some(actual.clone()),
                        error: format!(
                            "antigravity changed continuation session from {expected:?} to {actual:?}"
                        ),
                        usage: turn.usage,
                    }));
                }
                (Some(expected), _) => Some(expected.clone()),
                (None, actual) => actual,
            };

            let summary = match turn.structured_output {
                Some(value) => serde_json::to_string(&value).map_err(|e| {
                    Box::new(DetailedError::Other {
                        session: session.clone(),
                        error: format!("failed to encode structured output: {e}"),
                        usage: turn.usage.clone(),
                    })
                })?,
                None => turn
                    .result
                    .filter(|s| !s.trim().is_empty())
                    .ok_or_else(|| {
                        Box::new(DetailedError::Other {
                            session: session.clone(),
                            error: "antigravity returned an empty job summary".to_owned(),
                            usage: turn.usage.clone(),
                        })
                    })?,
            };

            let executed = (ver >= 3).then(|| Executed {
                model: resolved_model,
                effort: resolved_effort.map(|e| e.as_str().to_owned()),
                context,
            });

            Ok((
                Response {
                    contract: request.contract.clone(),
                    by: Identity {
                        engine: ENGINE.to_owned(),
                        session,
                    },
                    execution: executed,
                    answer: Answer::Job {
                        summary,
                        next: Next::Done,
                    },
                },
                turn.usage,
            ))
        }
        _ => Err(DetailedError::other(
            "antigravity plugin serves describe and job requests alone".to_owned(),
        )),
    }
}

fn resolve_execution(
    config: &Config,
    offers: &Offers,
    request: &Request,
    task_model: Option<&str>,
    task_effort: Option<&str>,
) -> Result<(Option<String>, Option<Effort>), String> {
    let choice = request.execution.as_ref().and_then(|e| e.choice.as_ref());
    if let Some(c) = choice
        && c.engine != ENGINE
    {
        return Err(format!(
            "execution choice targets engine {:?}, not {ENGINE:?}",
            c.engine
        ));
    }
    let choice_model = choice.and_then(|c| c.model.as_deref());
    let choice_effort = choice.and_then(|c| c.effort.as_deref());

    if let (Some(tm), Some(cm)) = (task_model, choice_model)
        && tm != cm
    {
        return Err(format!(
            "job model {tm:?} disagrees with execution choice model {cm:?}"
        ));
    }
    if let (Some(te), Some(ce)) = (task_effort, choice_effort)
        && te != ce
    {
        return Err(format!(
            "job effort {te:?} disagrees with execution choice effort {ce:?}"
        ));
    }

    let requested_model = task_model.or(choice_model);
    let requested_effort = task_effort.or(choice_effort);

    if requested_model.is_none() && requested_effort.is_some() {
        return Err("effort cannot be requested without a model".to_owned());
    }

    let model = match requested_model {
        Some(m) => {
            if !offers.models.iter().any(|offered| offered.id == m) {
                return Err(format!("model {m:?} is not offered by this plugin"));
            }
            Some(m.to_owned())
        }
        None if offers.models.is_empty() => None,
        None => {
            let chosen = config
                .default_model
                .as_deref()
                .filter(|d| offers.models.iter().any(|m| m.id == *d))
                .unwrap_or(&offers.models[0].id);
            Some(chosen.to_owned())
        }
    };

    let effort = match requested_effort {
        Some(raw) => {
            let parsed = Effort::parse(raw)
                .ok_or_else(|| format!("unsupported antigravity effort {raw:?}"))?;
            if !offers.allows(model.as_deref(), Some(parsed.as_str())) {
                return Err(format!(
                    "effort {:?} is not offered for model {:?}",
                    parsed.as_str(),
                    model
                ));
            }
            Some(parsed)
        }
        None => config
            .default_effort
            .filter(|e| offers.allows(model.as_deref(), Some(e.as_str()))),
    };

    Ok((model, effort))
}
