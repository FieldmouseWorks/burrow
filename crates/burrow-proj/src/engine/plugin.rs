//! Conary `conary-plugin v5` (`job`) runner for `proj`.
//!
//! Every `proj` turn executes through an external engine plugin speaking
//! `conary-plugin v5` on stdin/stdout: `describe` negotiates the contract,
//! stable plugin identity, offered models/efforts, capabilities, and meters;
//! `Task::Job` executes the turn under `timeout(1)` with `Execution` and
//! `Negotiated` provenance verification. Optional structured JSON lines on
//! stderr (`stream`, `session`, `activity`, `usage`, `refusal`, `error`)
//! supply live progress, normalized usage, and provider-owned refusal
//! classification without any provider-specific logic in `burrow-proj`.

use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime};

use conary_core::plugin::{
    Answer, CONTRACT_V1, CONTRACTS, Context, Execution, Kind, Negotiated, Next, Offers, Request,
    Response, Task, version,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Choice, capabilities, machine, stream};
use crate::config::Config;
use crate::job::Job;
use crate::record::{JobFile, TurnFile};
use crate::scratch;

mod launch;

/// Maximum `describe` or `job` stdout response size (32 MiB).
const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
/// Maximum `describe` wall-clock timeout in seconds.
const DESCRIBE_TIMEOUT_SECS: &str = "30s";

/// Negotiated metadata returned by a plugin's `describe` call.
#[derive(Clone, Debug)]
pub struct Described {
    pub contract: String,
    pub version: u8,
    pub negotiated: Negotiated,
}

impl Described {
    pub fn uses_credit(&self) -> bool {
        self.negotiated.offers.meters.iter().any(|m| m == "credit")
    }
}

/// Structured refusal kind reported by an engine plugin on stderr.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalKind {
    UsageLimit,
    LoginExpired,
    ApiKeyRejected,
    BudgetExhausted,
    CreditExhausted,
}

/// Structured refusal reported by an engine plugin on stderr.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct PluginRefusal {
    pub kind: RefusalKind,
    pub reason: String,
    #[serde(default)]
    pub resets_at: Option<i64>,
}

/// Normalized token/cost usage emitted by an engine plugin on stderr.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct PluginUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub total_cost_usd: Option<f64>,
}

/// Structured stderr diagnostic event emitted by Conary engine plugins.
#[derive(Debug, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum DiagnosticEvent {
    Stream {
        line: String,
    },
    Session {
        session_id: String,
    },
    Activity,
    Usage {
        #[serde(default)]
        session: Option<String>,
        usage: PluginUsage,
    },
    Refusal {
        #[serde(default)]
        session: Option<String>,
        refusal: PluginRefusal,
        #[serde(default)]
        usage: Option<PluginUsage>,
    },
    Error {
        #[serde(default)]
        session: Option<String>,
        error: String,
        #[serde(default)]
        usage: Option<PluginUsage>,
    },
}

#[derive(Default)]
struct CollectedStderr {
    usage: Option<PluginUsage>,
    refusal: Option<PluginRefusal>,
    error: Option<String>,
    session: Option<String>,
    streamed_lines: usize,
    plain_tail: Vec<String>,
}

/// Outcome of one completed plugin invocation, after response validation and
/// diagnostic processing.
#[derive(Debug)]
pub struct RunResult {
    pub last: Option<Value>,
    pub refusal: Option<PluginRefusal>,
    pub rc: i32,
}

/// Why [`spawn`] started nothing.
#[derive(Debug)]
pub enum SpawnError {
    /// The turn may not run on this engine where it is: the route takes it
    /// on as it does a run the plugin refused.
    Refused(PluginRefusal),
    Failed(String),
}

impl From<String> for SpawnError {
    fn from(e: String) -> Self {
        Self::Failed(e)
    }
}

impl std::fmt::Display for SpawnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(r) => f.write_str(&r.reason),
            Self::Failed(e) => f.write_str(e),
        }
    }
}

/// A live running plugin subprocess, together with its stdio threads and
/// negotiated contract state for post-exit provenance verification.
pub struct Running {
    pub child: Option<Child>,
    described: Described,
    execution: Execution,
    choice: Choice,
    started_written: bool,
    started_at: i64,
    stdin_handle: Option<JoinHandle<()>>,
    stdout_handle: Option<JoinHandle<io::Result<Vec<u8>>>>,
    stderr_handle: Option<JoinHandle<CollectedStderr>>,
}

/// Call `describe` on `plugin_path` and verify `conary-plugin` contract
/// compliance, stable `by.engine` identity, `Kind::Job` support, and `Offers`.
pub fn describe(cfg: &Config, plugin_path: &Path, engine: &str) -> Result<Described, String> {
    let req_bytes = serde_json::to_vec(&Request::describe())
        .map_err(|e| format!("failed to encode describe request: {e}"))?;
    let mut cmd = Command::new(&cfg.tools.timeout);
    cmd.args(["-k", "10s", DESCRIBE_TIMEOUT_SECS])
        .arg(plugin_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| {
        format!(
            "couldn't start {engine} plugin {}: {e}",
            plugin_path.display()
        )
    })?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin
            .write_all(&req_bytes)
            .and_then(|()| stdin.write_all(b"\n"));
    }
    let mut stdout = Vec::new();
    if let Some(out) = child.stdout.take() {
        out.take(MAX_RESPONSE_BYTES as u64 + 1)
            .read_to_end(&mut stdout)
            .map_err(|e| format!("{engine} plugin describe stdout read failed: {e}"))?;
    }
    let mut stderr = Vec::new();
    if let Some(err) = child.stderr.take() {
        let _ = err.take(64 * 1024).read_to_end(&mut stderr);
    }
    let status = child
        .wait()
        .map_err(|e| format!("{engine} plugin describe wait failed: {e}"))?;
    if !status.success() {
        let detail = String::from_utf8_lossy(&stderr).trim().to_owned();
        return Err(if detail.is_empty() {
            format!("{engine} plugin describe exited unsuccessfully ({status})")
        } else {
            format!("{engine} plugin describe failed: {detail}")
        });
    }
    if stdout.len() > MAX_RESPONSE_BYTES {
        return Err(format!(
            "{engine} plugin describe response exceeded size cap"
        ));
    }
    parse_describe(engine, &stdout)
}

fn parse_describe(engine: &str, stdout: &[u8]) -> Result<Described, String> {
    let response: Response = serde_json::from_slice(stdout)
        .map_err(|e| format!("{engine} plugin describe returned invalid JSON: {e}"))?;
    if response.contract != CONTRACT_V1 {
        return Err(format!(
            "{engine} plugin describe used contract {:?}, expected {CONTRACT_V1:?}",
            response.contract
        ));
    }
    if response.by.engine != engine {
        return Err(format!(
            "{engine} plugin describe reported engine identity {:?}, expected {engine:?}",
            response.by.engine
        ));
    }
    let Answer::Describe {
        versions,
        kinds,
        v2_kinds,
        v4_kinds,
        models,
        capabilities,
        meters,
    } = response.answer
    else {
        return Err(format!(
            "{engine} plugin describe returned wrong answer kind"
        ));
    };
    let contract = CONTRACTS
        .iter()
        .rev()
        .find(|c| versions.iter().any(|v| v == **c))
        .copied()
        .ok_or_else(|| format!("{engine} plugin supports no compatible conary-plugin contract"))?;
    let ver = version(contract).ok_or_else(|| format!("unknown contract {contract:?}"))?;
    if ver < 2 {
        return Err(format!(
            "{engine} plugin contract {contract:?} does not support job tasks"
        ));
    }
    let all_kinds: Vec<Kind> = kinds.into_iter().chain(v2_kinds).chain(v4_kinds).collect();
    if !all_kinds.contains(&Kind::Job) {
        return Err(format!("{engine} plugin does not serve the job kind"));
    }
    let capabilities =
        capabilities.ok_or_else(|| format!("{engine} plugin describe omitted capabilities"))?;
    let offers = Offers {
        models,
        capabilities,
        meters,
    };
    if !offers.valid(&all_kinds) {
        return Err(format!("{engine} plugin describe returned invalid offers"));
    }
    Ok(Described {
        contract: contract.to_owned(),
        version: ver,
        negotiated: Negotiated {
            by: response.by,
            offers,
        },
    })
}

/// Resolve the effective `(model, effort)` against negotiated `offers`,
/// enforcing `conary-plugin v5`'s `Offers::allows` invariant.
fn resolve_model_effort(
    engine: &str,
    offers: &Offers,
    choice: &Choice,
) -> Result<(Option<String>, Option<String>), String> {
    match (choice.model.as_deref(), choice.effort.as_deref()) {
        (None, None) => Ok((None, None)),
        (Some(m), e) => {
            if !offers.allows(Some(m), e) {
                return Err(format!(
                    "{engine} plugin does not offer model {m:?}{}",
                    e.map_or(String::new(), |ef| format!(" with effort {ef:?}"))
                ));
            }
            Ok((Some(m.to_owned()), e.map(str::to_owned)))
        }
        (None, Some(e)) => {
            if offers.models.len() == 1 && offers.allows(Some(&offers.models[0].id), Some(e)) {
                Ok((Some(offers.models[0].id.clone()), Some(e.to_owned())))
            } else {
                Err(format!(
                    "{engine} plugin requires a model to select effort {e:?}"
                ))
            }
        }
    }
}

/// Start one `conary-plugin v5` (`job`) run for `job` turn `n`: on the host,
/// or, given its sandbox (`agent`), inside that sandbox ([`launch`]).
#[allow(clippy::too_many_arguments)]
pub fn spawn(
    cfg: &Config,
    job: &Job,
    n: &str,
    prompt_path: &Path,
    waitdir: &Path,
    agent: Option<&str>,
    must: bool,
    app_env: &str,
    choice: &Choice,
) -> Result<Running, SpawnError> {
    let plugin_path = cfg
        .plugins
        .get(&choice.engine)
        .ok_or_else(|| format!("no engine plugin configured for {}", choice.engine))?;

    // The plugin a sandboxed turn runs is this same executable (the sandbox
    // shares the host's store), so its describe is this one.
    let mut described = describe(cfg, plugin_path, &choice.engine)?;
    // A credit is admitted and spent through its ledger on the host, which a
    // sandbox's root guest is never given: a credit-metered engine doesn't
    // run in a sandbox, and the turn's route takes it on.
    if agent.is_some() && described.uses_credit() {
        return Err(SpawnError::Refused(PluginRefusal {
            kind: RefusalKind::CreditExhausted,
            reason: format!(
                "{} runs on a credit, which a sandbox can't be admitted to, so it doesn't run there",
                choice.engine
            ),
            resets_at: None,
        }));
    }
    // When a plugin leaves its static `describe` model catalog empty (meaning
    // unpinned turns run on the harness's own default without `--model`), an
    // explicit per-role model override admitted by `burrow-settings`'s live
    // `proj.engineOffers` is carried into the v5 `Negotiated` snapshot only for
    // turns that actually select that model.
    if described.negotiated.offers.models.is_empty()
        && let Some(model_id) = choice.model.as_deref()
        && let Some(offer) = cfg
            .settings
            .as_ref()
            .and_then(|s| s.role_view().ok())
            .and_then(|v| v.in_force.offers.get(&choice.engine).cloned())
        && offer.models.iter().any(|m| m == model_id)
    {
        described
            .negotiated
            .offers
            .models
            .push(conary_core::plugin::Model {
                id: model_id.to_owned(),
                efforts: offer.efforts,
                context_tokens: None,
                max_output_tokens: None,
            });
    }
    let (model, effort) =
        resolve_model_effort(&choice.engine, &described.negotiated.offers, choice)?;

    let request_record = crate::job::Request::load(job)?;
    let prompt_text = String::from_utf8_lossy(
        &crate::state_reads::bytes(prompt_path)
            .map_err(|e| format!("{}: {e}", prompt_path.display()))?,
    )
    .into_owned();
    let repo = {
        let gh = crate::pick::github_repo(cfg, &request_record.project);
        if !gh.is_empty() {
            gh
        } else {
            Path::new(&request_record.project)
                .file_name()
                .and_then(|s| s.to_str())
                .filter(|s| !s.trim().is_empty())
                .unwrap_or("local/project")
                .to_owned()
        }
    };
    let context = match stream::resume_for(job, &choice.engine) {
        Some(session) => Context::Continue { session },
        None => Context::Fresh,
    };
    let execution = Execution {
        choice: Some(conary_core::plugin::Choice {
            engine: choice.engine.clone(),
            model: model.clone(),
            effort: effort.clone(),
        }),
        context,
    };
    let wire_request = Request {
        contract: described.contract.clone(),
        execution: (described.version >= 3).then(|| execution.clone()),
        negotiated: (described.version >= 5).then(|| described.negotiated.clone()),
        task: Task::Job {
            repo,
            workdir: request_record.project.clone(),
            prompt: prompt_text,
            model: model.clone(),
            effort: effort.clone(),
        },
    };
    let request_bytes = serde_json::to_vec(&wire_request)
        .map_err(|e| format!("failed to encode job request: {e}"))?;

    let stream_path = job.turn_file(n, TurnFile::Jsonl);
    let mut cmd = Command::new(&cfg.tools.timeout);
    cmd.args(["-k", "1m", "0"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("PROJ_STREAM_EVENTS", "1");

    match agent {
        Some(agent) => launch::sandboxed(&mut cmd, job, n, agent, must, plugin_path),
        None => {
            launch::host(&mut cmd, cfg, job, n, waitdir, must, app_env, plugin_path);
            if described.uses_credit() {
                cmd.env("PROJ_CREDIT_LEDGER_FILE", &stream_path);
            }
        }
    }

    let started_at = crate::now();
    let uses_credit = described.uses_credit();
    let mut child = if uses_credit {
        // Credit-metered plugins write their own atomic budgeted `burrow.run`
        // marker under `.credit.lock` once admitted; avoid writing a duplicate
        // unbudgeted marker or `N.started` before admission succeeds.
        cmd.spawn()
            .map_err(|e| format!("couldn't start {} plugin: {e}", choice.engine))?
    } else {
        job.start_budgeted_run(
            n,
            started_at,
            &choice.engine,
            None,
            choice.model.as_deref(),
            || cmd.spawn(),
        )
        .map_err(|e| format!("couldn't start {} plugin: {e}", choice.engine))?
    };

    let stdin_handle = child.stdin.take().map(|mut stdin| {
        thread::spawn(move || {
            let _ = stdin
                .write_all(&request_bytes)
                .and_then(|()| stdin.write_all(b"\n"));
        })
    });
    let stdout_handle = child.stdout.take().map(|out| {
        thread::spawn(move || {
            let mut buf = Vec::new();
            out.take(MAX_RESPONSE_BYTES as u64 + 1)
                .read_to_end(&mut buf)?;
            Ok(buf)
        })
    });
    let stderr_handle = child.stderr.take().map(|err| {
        let stream_out = stream_path.clone();
        let job_dir = job.dir.clone();
        let engine_name = choice.engine.clone();
        thread::spawn(move || pump_stderr(err, &stream_out, &job_dir, &engine_name))
    });

    Ok(Running {
        child: Some(child),
        described,
        execution,
        choice: choice.clone(),
        started_written: !uses_credit,
        started_at,
        stdin_handle,
        stdout_handle,
        stderr_handle,
    })
}

/// Consume plugin stderr line by line: forward structured `stream`/`session`/
/// `activity` events into `N.jsonl` in real time, capture terminal `usage`/
/// `refusal`/`error` diagnostics, and forward plain stderr lines to our stderr.
fn pump_stderr(
    stderr: impl Read,
    stream_path: &Path,
    job_dir: &Path,
    engine_name: &str,
) -> CollectedStderr {
    let mut collected = CollectedStderr::default();
    let reader = BufReader::new(stderr);
    for line in reader.lines().map_while(Result::ok) {
        let trimmed = line.trim();
        if trimmed.starts_with('{')
            && let Ok(event) = serde_json::from_str::<DiagnosticEvent>(trimmed)
        {
            match event {
                DiagnosticEvent::Stream { line: raw } => {
                    append_line(stream_path, &raw);
                    collected.streamed_lines += 1;
                }
                DiagnosticEvent::Session { session_id } => {
                    if !session_id.trim().is_empty() {
                        collected.session = Some(session_id.clone());
                        append_line(
                            stream_path,
                            &json!({
                                "type": "system",
                                "subtype": "init",
                                "session_id": session_id,
                            })
                            .to_string(),
                        );
                        persist_session_in_dir(job_dir, engine_name, &session_id);
                    }
                }
                DiagnosticEvent::Activity => {
                    touch_file(stream_path);
                }
                DiagnosticEvent::Usage { session, usage } => {
                    if collected.session.is_none() {
                        collected.session = session;
                    }
                    collected.usage = Some(usage);
                }
                DiagnosticEvent::Refusal {
                    session,
                    refusal,
                    usage,
                } => {
                    if collected.session.is_none() {
                        collected.session = session;
                    }
                    if let Some(u) = usage {
                        collected.usage = Some(u);
                    }
                    collected.refusal = Some(refusal);
                }
                DiagnosticEvent::Error {
                    session,
                    error,
                    usage,
                } => {
                    if collected.session.is_none() {
                        collected.session = session;
                    }
                    if let Some(u) = usage {
                        collected.usage = Some(u);
                    }
                    collected.error = Some(error);
                }
            }
            continue;
        }
        if !trimmed.is_empty() {
            eprintln!("{line}");
            if collected.plain_tail.len() >= 10 {
                collected.plain_tail.remove(0);
            }
            collected.plain_tail.push(line);
        }
    }
    collected
}

fn persist_session_in_dir(job_dir: &Path, engine: &str, id: &str) {
    let conv = super::conversation_of(engine);
    let session_engine = job_dir.join("session.engine");
    let session_file = job_dir.join("session");
    if !session_engine.exists()
        && let Some(prev) = crate::read(&session_file)
        && !prev.is_empty()
    {
        let _ = crate::state_reads::write(
            job_dir.join(format!("session.{}", super::DEFAULT_ENGINE)),
            format!("{prev}\n"),
        );
    }
    let _ = crate::state_reads::write(job_dir.join(format!("session.{conv}")), format!("{id}\n"));
    let _ = crate::state_reads::write(&session_file, format!("{id}\n"));
    let _ = crate::state_reads::write(&session_engine, conv);
}

fn append_line(path: &Path, line: &str) {
    if let Ok(mut f) = crate::state_reads::append(path) {
        let _ = writeln!(f, "{line}");
    }
}

fn touch_file(path: &Path) {
    if let Ok(f) = crate::state_reads::append(path) {
        let _ = f.set_modified(SystemTime::now());
    }
}

impl Running {
    /// Complete the run after `watch` observes process exit, validate the
    /// `conary-plugin v5` response on stdout, persist session/result/refusal
    /// records to `N.jsonl`, and return the structured outcome.
    pub fn finish(
        mut self,
        status: ExitStatus,
        cut: bool,
        cfg: &Config,
        job: &Job,
        n: &str,
        from: u64,
    ) -> RunResult {
        if let Some(h) = self.stdin_handle.take() {
            let _ = h.join();
        }
        let stdout = self
            .stdout_handle
            .take()
            .and_then(|h| h.join().ok())
            .and_then(Result::ok)
            .unwrap_or_default();
        let stderr = self
            .stderr_handle
            .take()
            .and_then(|h| h.join().ok())
            .unwrap_or_default();

        let stream_path = job.turn_file(n, TurnFile::Jsonl);
        let rc = exit_code(status);

        // Credit-metered runs write `N.started` only once the plugin actually
        // ran (stream grew past `from` or plugin succeeded), never on pre-start
        // credit/key refusal.
        let stream_len = fs::metadata(&stream_path).map_or(0, |m| m.len());
        if !self.started_written && (stream_len > from || status.success()) {
            let started = job.turn_file(n, TurnFile::Started);
            if !started.exists() {
                let _ = crate::state_reads::write(started, format!("{}\n", self.started_at));
            }
        }

        if let Some(sid) = stderr.session.as_deref() {
            stream::record_session(job, &self.choice.engine, sid);
        }

        if let Some(refusal) = stderr.refusal {
            // Persist normalized refusal record for deploy-overlap checkpoint
            // recovery (`retry.rs`) when this run already wrote to `N.jsonl`.
            if stream_len > from {
                append_line(
                    &stream_path,
                    &json!({
                        "type": "burrow.refusal",
                        "kind": match refusal.kind {
                            RefusalKind::UsageLimit => "usage_limit",
                            RefusalKind::LoginExpired => "login_expired",
                            RefusalKind::ApiKeyRejected => "api_key_rejected",
                            RefusalKind::BudgetExhausted => "budget_exhausted",
                            RefusalKind::CreditExhausted => "credit_exhausted",
                        },
                        "reason": refusal.reason,
                        "resets_at": refusal.resets_at,
                    })
                    .to_string(),
                );
            }
            return RunResult {
                last: stream::last_result(&stream_path, from),
                refusal: Some(refusal),
                rc: if rc == 0 { 1 } else { rc },
            };
        }

        // A lingering run cut by the 10s watcher keeps whatever answer it
        // already streamed before SIGTERM.
        if cut {
            return RunResult {
                last: stream::last_result(&stream_path, from),
                refusal: None,
                rc,
            };
        }

        if status.success() && !stdout.is_empty() {
            match self.verify_response(&stdout) {
                Ok((session, summary, next)) => {
                    if let Some(sid) = session.as_deref() {
                        stream::record_session(job, &self.choice.engine, sid);
                    }
                    if stream::last_result(&stream_path, from).is_none() {
                        let mut record = json!({
                            "type": "result",
                            "subtype": "success",
                            "is_error": false,
                            "result": summary,
                        });
                        if let Some(sid) = session.as_deref() {
                            record["session_id"] = Value::from(sid);
                        }
                        if let Some(u) = &stderr.usage {
                            record["usage"] = json!({
                                "input_tokens": u.input_tokens,
                                "output_tokens": u.output_tokens,
                                "cache_read_input_tokens": u.cache_read_input_tokens,
                                "cache_creation_input_tokens": u.cache_creation_input_tokens,
                            });
                            if let Some(cost) = u.total_cost_usd {
                                record["total_cost_usd"] = Value::from(cost);
                            }
                        }
                        append_line(&stream_path, &record.to_string());
                    }
                    apply_protocol_next(cfg, job, &next);
                    return RunResult {
                        last: stream::last_result(&stream_path, from),
                        refusal: None,
                        rc: 0,
                    };
                }
                Err(why) => {
                    eprintln!(
                        "proj: {} plugin response rejected: {why}",
                        self.choice.engine
                    );
                    return RunResult {
                        last: None,
                        refusal: None,
                        rc: 1,
                    };
                }
            }
        }

        // Non-zero exit without a structured refusal: log any structured error
        // reported by the plugin, but leave `last` to what the harness actually
        // emitted so crashed or OOM-killed runs (`last == None`) still trigger
        // OOM retry or record `failed` rather than masquerading as an `error`
        // result with exit 0.
        if let Some(err) = stderr.error {
            eprintln!("proj: {} plugin error: {err}", self.choice.engine);
        }
        RunResult {
            last: stream::last_result(&stream_path, from),
            refusal: None,
            rc,
        }
    }

    fn verify_response(&self, stdout: &[u8]) -> Result<(Option<String>, String, Next), String> {
        if stdout.len() > MAX_RESPONSE_BYTES {
            return Err("plugin response exceeded size cap".to_owned());
        }
        let response: Response = serde_json::from_slice(stdout)
            .map_err(|e| format!("invalid plugin response JSON: {e}"))?;
        if response.contract != self.described.contract {
            return Err(format!(
                "response contract {:?} disagrees with negotiated {:?}",
                response.contract, self.described.contract
            ));
        }
        if response.by.engine != self.described.negotiated.by.engine {
            return Err(format!(
                "response engine {:?} disagrees with described {:?}",
                response.by.engine, self.described.negotiated.by.engine
            ));
        }
        if self.described.version >= 5 {
            let provenance = response
                .provenance()
                .ok_or_else(|| "v5 plugin response omitted execution provenance".to_owned())?;
            self.described
                .negotiated
                .verify(&self.execution, &provenance)
                .map_err(|e| format!("v5 provenance verification failed: {e}"))?;
        }
        let Answer::Job { summary, next } = response.answer else {
            return Err("plugin response returned wrong answer kind for job".to_owned());
        };
        if !next.valid() {
            return Err("plugin response returned invalid next step".to_owned());
        }
        Ok((response.by.session, summary, next))
    }
}

/// Apply structured `Answer::Job.next` declarations (`Await` / `Ask`) when the
/// turn did not already declare a next step through `proj await|ask|done`.
/// `Next::Done` is intentionally left to `proj done` for non-`--owned` jobs so
/// silent turns still trigger the undeclared-next-step check.
fn apply_protocol_next(cfg: &Config, job: &Job, next: &Next) {
    if crate::filled(&job.file(JobFile::EndNext)) {
        return;
    }
    match next {
        Next::Done => {}
        Next::Await { url } => {
            if let Some(target) = crate::github::target(url) {
                let _ =
                    crate::cmd::record_wait(cfg, job, &[target], std::slice::from_ref(url), None);
            }
        }
        Next::Ask {
            default, one_way, ..
        } => {
            let rec = crate::ask::Recommendation {
                default: default.clone(),
                one_way: *one_way,
                about: None,
            };
            if crate::ask::record(job, &rec).is_ok() {
                let _ = fs::remove_file(job.file(JobFile::AwaitNext));
                let _ = crate::ask::declare(job, "ask");
            }
        }
    }
}

fn exit_code(status: ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
}

/// Host-side plugin launcher (`proj _plugin PLUGIN_PATH`): resolves the job's
/// host capability grants in the final login+App environment, exports them as
/// generic `PROJ_*` variables, and `exec`s the plugin executable with stdio
/// untouched.
pub fn exec_plugin(cfg: &Config, args: &[String]) -> Result<(), crate::Fail> {
    use std::os::unix::process::CommandExt;
    let plugin_path = args
        .first()
        .filter(|p| !p.is_empty())
        .ok_or_else(|| crate::Fail::new(2, "usage: proj _plugin PATH"))?;
    let id = std::env::var("PROJ_JOB").map_err(|e| crate::Fail::new(1, e.to_string()))?;
    let caps = capabilities::resolve(
        cfg,
        &Job::new(cfg, &id),
        &scratch::dir(cfg, &id),
        &|name| std::env::var_os(name),
        &machine::Machine::real(),
    )
    .map_err(|e| crate::Fail::new(1, e.to_string()))?;

    let mut command = Command::new(plugin_path);
    if !caps.inherits_environment() {
        command.env_clear();
    }
    for (k, v) in caps.environment() {
        command.env(k, v);
    }
    let grant_paths: Vec<(PathBuf, bool)> = caps
        .items
        .iter()
        .flat_map(|g| g.paths.iter().cloned())
        .collect();
    if let Ok(encoded) = serde_json::to_string(&grant_paths) {
        command.env("PROJ_GRANT_PATHS", encoded);
    }
    let servers = caps.servers();
    if !servers.is_empty()
        && let Ok(encoded) = serde_json::to_string(&servers)
    {
        command.env("PROJ_MCP_SERVERS", encoded);
    }
    let error = command.exec();
    Err(crate::Fail::new(
        1,
        format!("couldn't start engine plugin {plugin_path}: {error}"),
    ))
}

/// Wait briefly for a `describe` call's duration helper (unused outside tests).
#[allow(dead_code)]
const _KEEP_DURATION: Duration = Duration::from_secs(1);

#[cfg(test)]
mod tests;
