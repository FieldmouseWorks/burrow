//! Conary `conary-plugin v5` (`describe` + `work` for `job`) handler.

use std::path::PathBuf;

use conary_core::plugin::{
    Answer, CONTRACT_V1, CONTRACTS, Context, Executed, Identity, Kind, Next, Offers, Request,
    Response, Task, fixtures, version,
};

use crate::cli::{Invocation, run_invocation};
use crate::config::{Config, ENGINE, Effort};

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

/// Answer one Conary plugin request without external cancellation.
pub fn answer(
    config: &Config,
    request: &Request,
    instructions: Option<&str>,
) -> Result<Response, String> {
    answer_cancellable(config, request, instructions, &|| false)
}

/// Answer one Conary plugin request, aborting and killing the child process
/// group as soon as `stop()` returns `true`. The turn's `instructions`
/// (burrow_grants::turn) go ahead of a job's prompt: the one input the CLI
/// is known to give the model.
pub fn answer_cancellable(
    config: &Config,
    request: &Request,
    instructions: Option<&str>,
    stop: &dyn Fn() -> bool,
) -> Result<Response, String> {
    config.validate()?;

    match &request.task {
        Task::Describe => Ok(describe(config)),
        Task::Job {
            workdir,
            prompt,
            model,
            effort,
            ..
        } => {
            let ver = version(&request.contract)
                .ok_or_else(|| format!("unsupported plugin contract {:?}", request.contract))?;
            if ver < 2 {
                return Err("the job kind requires conary-plugin v2 or newer".to_owned());
            }

            let offers = match &request.negotiated {
                Some(neg) => {
                    if neg.by.engine != ENGINE {
                        return Err(format!(
                            "negotiated engine {:?} does not match {ENGINE:?}",
                            neg.by.engine
                        ));
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
            )?;

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
                        return Err("antigravity plugin does not support fork context".to_owned());
                    }
                };
                return Ok(Response {
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
                });
            }

            let conversation = match &context {
                Context::Fresh => None,
                Context::Continue { session } => {
                    if session.trim().is_empty() {
                        return Err("continue context requires a nonblank session id".to_owned());
                    }
                    Some(session.clone())
                }
                Context::Fork { .. } => {
                    return Err("antigravity plugin does not support fork context".to_owned());
                }
            };

            let prompt = match instructions {
                Some(told) => format!("{told}\n\n{prompt}"),
                None => prompt.clone(),
            };
            let invocation = Invocation::from_config(
                config,
                PathBuf::from(workdir),
                prompt,
                resolved_model.clone(),
                resolved_effort,
                conversation.clone(),
            );
            let turn = run_invocation(config, &invocation, stop);

            if let Some(err) = turn.error {
                return Err(err.to_string());
            }

            let session = match (&conversation, turn.session) {
                (Some(expected), Some(actual)) if actual != *expected => {
                    return Err(format!(
                        "antigravity changed continuation session from {expected:?} to {actual:?}"
                    ));
                }
                (Some(expected), _) => Some(expected.clone()),
                (None, actual) => actual,
            };

            let summary = turn
                .result
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| "antigravity returned an empty job summary".to_owned())?;

            let executed = (ver >= 3).then(|| Executed {
                model: resolved_model,
                effort: resolved_effort.map(|e| e.as_str().to_owned()),
                context,
            });

            Ok(Response {
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
            })
        }
        _ => Err("antigravity plugin serves describe and job requests alone".to_owned()),
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
