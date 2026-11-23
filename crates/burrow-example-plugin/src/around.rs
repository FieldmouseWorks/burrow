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
