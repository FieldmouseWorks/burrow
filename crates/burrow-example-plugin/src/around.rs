//! The fixture put around another plugin (lib/engine-conformance.nix): that
//! plugin's describe and Conary's conformance jobs go to it as they came,
//! and a look is the fixture's, answered as that plugin. So a sandbox grants
//! the fixture what that plugin declares, in that plugin's engine, and the
//! fixture tries every capability there.

use conary_core::plugin::{Answer, Identity, Request, Response, Task};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

/// Whether the fixture answers `task` itself: a job that isn't Conary's.
pub fn looks(task: &Task) -> bool {
    matches!(task, Task::Job { .. }) && !crate::conformance(task)
}

/// `plugin`'s answer to `input`, onto stdout as it comes.
pub fn pass(plugin: &Path, input: &[u8]) -> Result<(), String> {
    let output = run(plugin, input, Stdio::inherit())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("{}: {}", plugin.display(), output.status))
    }
}

/// `response`, as `plugin`'s engine gave it: Conary's runner holds every
/// answer to the engine its describe named.
pub fn as_plugin(plugin: &Path, response: Response) -> Result<Response, String> {
    let request = serde_json::to_vec(&Request::describe()).map_err(|e| e.to_string())?;
    let output = run(plugin, &request, Stdio::piped())?;
    if !output.status.success() {
        return Err(format!(
            "{}'s describe: {}",
            plugin.display(),
            output.status
        ));
    }
    let described: Response = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("{}'s describe doesn't read: {e}", plugin.display()))?;
    Ok(adopt(&described, response))
}

/// Apply `described`'s engine identity and, for a model-offering plugin, its
/// first offered model so Conary's v3+ execution check accepts the fixture's
/// answer when wrapping a real engine plugin.
fn adopt(described: &Response, mut response: Response) -> Response {
    response.by = Identity {
        engine: described.by.engine.clone(),
        session: None,
    };
    if let (Some(executed), Answer::Describe { models, .. }) =
        (&mut response.execution, &described.answer)
        && executed.model.is_none()
        && let Some(first) = models.first()
    {
        executed.model = Some(first.id.clone());
    }
    response
}

fn run(plugin: &Path, input: &[u8], stdout: Stdio) -> Result<Output, String> {
    let fail = |e: std::io::Error| format!("{}: {e}", plugin.display());
    // Not the fixture's own setting: the plugin it's around is never it.
    let mut child = Command::new(plugin)
        .env_remove(crate::AROUND)
        .stdin(Stdio::piped())
        .stdout(stdout)
        .spawn()
        .map_err(fail)?;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    stdin.write_all(input).map_err(fail)?;
    drop(stdin);
    child.wait_with_output().map_err(fail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use conary_core::plugin::{
        CONTRACT, CONTRACT_V1, Capabilities, Context, Executed, Execution, Kind, Model, Negotiated,
        Next, Offers, Provenance, Sandbox,
    };

    fn described_with(models: Vec<Model>) -> Response {
        Response {
            contract: CONTRACT_V1.to_owned(),
            by: Identity {
                engine: "fixture-engine".to_owned(),
                session: None,
            },
            execution: None,
            answer: Answer::Describe {
                versions: vec![CONTRACT_V1.to_owned(), CONTRACT.to_owned()],
                kinds: vec![],
                v2_kinds: vec![Kind::Job],
                v4_kinds: vec![],
                models,
                capabilities: Some(Capabilities {
                    sandbox: Sandbox::Needs,
                    network: true,
                    devices: vec![],
                    mcp: false,
                }),
                meters: vec![],
            },
        }
    }

    fn job_response() -> Response {
        Response {
            contract: CONTRACT.to_owned(),
            by: Identity {
                engine: "example".to_owned(),
                session: None,
            },
            execution: Some(Executed {
                model: None,
                effort: None,
                context: Context::Fresh,
            }),
            answer: Answer::Job {
                summary: "ok".to_owned(),
                next: Next::Done,
            },
        }
    }

    #[test]
    fn adopts_an_offered_model_when_wrapping_a_model_plugin() {
        let models = vec![Model {
            id: "offered-model".to_owned(),
            efforts: vec!["low".to_owned(), "high".to_owned()],
            context_tokens: None,
            max_output_tokens: None,
        }];
        let described = described_with(models.clone());
        let adopted = adopt(&described, job_response());
        assert_eq!(adopted.by.engine, "fixture-engine");
        assert_eq!(
            adopted.execution.as_ref().and_then(|e| e.model.as_deref()),
            Some("offered-model")
        );

        let negotiated = Negotiated {
            by: described.by,
            offers: Offers {
                models,
                capabilities: Capabilities {
                    sandbox: Sandbox::Needs,
                    network: true,
                    devices: vec![],
                    mcp: false,
                },
                meters: vec![],
            },
        };
        let requested = Execution {
            choice: None,
            context: Context::Fresh,
        };
        let provenance = Provenance {
            by: adopted.by,
            execution: adopted.execution.unwrap(),
        };
        negotiated.verify(&requested, &provenance).unwrap();
    }

    #[test]
    fn leaves_model_none_when_wrapping_a_program_plugin() {
        let described = described_with(vec![]);
        let adopted = adopt(&described, job_response());
        assert_eq!(adopted.by.engine, "fixture-engine");
        assert_eq!(
            adopted.execution.as_ref().and_then(|e| e.model.as_deref()),
            None
        );
    }
}
