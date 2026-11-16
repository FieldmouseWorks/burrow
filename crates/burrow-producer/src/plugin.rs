//! The plugin: the executable the flow configuration names. A thin client
//! (Conary's `docs/m6-plugins.md`, "The plugin"): it reads the flow's one
//! request, answers `describe` and the reserved conformance fixtures
//! itself with no network, forwards any other work to its producer and
//! writes the producer's answer. Any producer error is an error here, which
//! the plugin's `main` turns into a nonzero exit: Conary takes that as the
//! plugin failing, never as a verdict.
use crate::serve::Work;
use crate::wire::{self, Reply};
use conary_core::plugin::{
    Answer, Executed, Execution, Identity, Request, Response, Task, fixtures, version,
};
use std::io::{BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

/// One plugin: what it says it is, how it answers the fixtures, and where
/// its producer listens.
pub struct Plugin {
    /// Its `describe` answer: the contracts it speaks, its kinds, and no
    /// models for a program.
    pub describe: Answer,
    /// Who its answers to `describe` and the fixtures are by.
    pub by: Identity,
    /// Its deterministic answer to a conformance fixture of a kind it
    /// serves, or None for a kind it doesn't. It contacts nothing and runs
    /// nothing.
    pub fixture: fn(&Task) -> Option<Answer>,
    /// Its producer's socket.
    pub socket: PathBuf,
}

impl Plugin {
    /// Read one request from `input` and write one answer to `output`, or
    /// say why there's none.
    pub fn answer(&self, input: &mut dyn Read, output: &mut dyn Write) -> Result<(), String> {
        let mut bytes = Vec::new();
        input
            .take(wire::MOST)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("couldn't read the request: {e}"))?;
        let request: Request =
            serde_json::from_slice(&bytes).map_err(|e| format!("the request doesn't read: {e}"))?;
        if version(&request.contract).is_none() {
            return Err(format!(
                "contract {:?} isn't one it speaks",
                request.contract
            ));
        }
        let response = match self.local(&request)? {
            Some(answer) => Response {
                contract: request.contract.clone(),
                by: self.by.clone(),
                execution: request.execution.as_ref().map(|e| self.executed(e)),
                answer,
            },
            None => self.forward(&request)?,
        };
        serde_json::to_writer(&mut *output, &response)
            .map_err(|e| format!("couldn't write the answer: {e}"))?;
        output
            .flush()
            .map_err(|e| format!("couldn't write the answer: {e}"))
    }

    /// What its own answers report as run: the model and effort the flow
    /// chose, or else its first offered model (a program offers none), in
    /// the context asked for. They run no model, but Conary holds every
    /// answer to the execution it asked for and the offers it was made.
    fn executed(&self, asked: &Execution) -> Executed {
        let offered = match &self.describe {
            Answer::Describe { models, .. } => models.first().map(|m| m.id.clone()),
            _ => None,
        };
        let choice = asked.choice.as_ref();
        Executed {
            model: choice.and_then(|c| c.model.clone()).or(offered),
            effort: choice.and_then(|c| c.effort.clone()),
            context: asked.context.clone(),
        }
    }

    /// The answer it gives itself: `describe`, and a fixture on the
    /// reserved repository. None for work its producer does.
    fn local(&self, request: &Request) -> Result<Option<Answer>, String> {
        if matches!(request.task, Task::Describe) {
            return Ok(Some(self.describe.clone()));
        }
        // The fixtures all name Conary's one reserved repository.
        let reserved = fixtures()
            .into_iter()
            .find_map(|f| repo(&f.task).map(str::to_owned));
        if reserved.is_none() || repo(&request.task) != reserved.as_deref() {
            return Ok(None);
        }
        (self.fixture)(&request.task)
            .map(Some)
            .ok_or_else(|| "it serves no fixture of this kind".into())
    }

    /// The producer's answer to `request`.
    fn forward(&self, request: &Request) -> Result<Response, String> {
        exchange(&self.socket, request, None)
    }
}

/// The answer of the server at `socket` (a producer's, or a producer's own
/// helper's) to `request`, on behalf of `work`: the connection is shut as
/// soon as `work` is abandoned, so the server stops what it does for it.
pub fn forward(socket: &Path, request: &Request, work: &Work) -> Result<Response, String> {
    exchange(socket, request, Some(work))
}

/// `request`'s answer from `socket`. The connection stays open until the
/// answer comes, and it ending first is the server's error: the server
/// reads it ending as its asker gone, and stops the work. With `work`, it's
/// shut once that's abandoned.
fn exchange(socket: &Path, request: &Request, work: Option<&Work>) -> Result<Response, String> {
    let stream = UnixStream::connect(socket)
        .map_err(|e| format!("couldn't reach the producer at {}: {e}", socket.display()))?;
    let done = Arc::new(AtomicBool::new(false));
    let watch = match work {
        Some(work) => {
            let (work, done, stream) = (
                work.clone(),
                done.clone(),
                stream
                    .try_clone()
                    .map_err(|e| format!("the connection can't be watched: {e}"))?,
            );
            Some(thread::spawn(move || {
                while !done.load(Ordering::SeqCst) {
                    if work.abandoned() {
                        let _ = stream.shutdown(std::net::Shutdown::Both);
                        return;
                    }
                    thread::sleep(Duration::from_millis(50));
                }
            }))
        }
        None => None,
    };
    let answered = answer_on(&stream, request);
    done.store(true, Ordering::SeqCst);
    if let Some(watch) = watch {
        let _ = watch.join();
    }
    if work.is_some_and(Work::abandoned) {
        return Err("its request was abandoned".into());
    }
    answered
}

fn answer_on(mut stream: &UnixStream, request: &Request) -> Result<Response, String> {
    // A producer that refuses may close before the whole request is
    // sent: its reason is still there to read, and says more.
    let sent = wire::send(&mut stream, request);
    let line = match (wire::line(&mut BufReader::new(stream)), sent) {
        (Ok(Some(line)), _) => line,
        (_, Err(e)) => return Err(format!("couldn't ask the producer: {e}")),
        (Err(e), Ok(())) => return Err(format!("couldn't read the producer's answer: {e}")),
        (Ok(None), Ok(())) => {
            return Err("the producer closed the connection without answering".into());
        }
    };
    let reply: Reply = serde_json::from_str(&line)
        .map_err(|e| format!("the producer's answer doesn't read: {e}"))?;
    match reply {
        Reply::Answer(response) if response.contract == request.contract => Ok(*response),
        Reply::Answer(response) => Err(format!(
            "the producer answered in {:?}, not {:?}",
            response.contract, request.contract
        )),
        Reply::Refused(why) => Err(format!("the producer refused: {why}")),
    }
}

/// The repository a work request names.
fn repo(task: &Task) -> Option<&str> {
    match task {
        Task::Describe => None,
        Task::Review { repo, .. }
        | Task::Fix { repo, .. }
        | Task::Judge { repo, .. }
        | Task::Proof { repo, .. }
        | Task::Prepare { repo, .. }
        | Task::Job { repo, .. } => Some(repo),
    }
}
