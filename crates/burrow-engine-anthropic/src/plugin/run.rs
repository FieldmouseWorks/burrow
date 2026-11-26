//! Subprocess execution, process-group cancellation cleanup, stream JSONL
//! parsing, usage extraction, and refusal/failure classification.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Child, ExitStatus, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use conary_core::plugin::Context;
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::process::{
    Pid, Signal, getpgid, getpid, kill_process, kill_process_group, test_kill_process_group,
};
use serde_json::Value;
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::Signals;

use super::{JobOutcome, JobUsage, PluginConfig, Refusal, RefusalKind};
use crate::{Mode, api, claude, command, stream};

const MAX_STDOUT_BYTES: usize = 64 * 1024 * 1024;
const MAX_STDERR_BYTES: usize = 1024 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(5);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);

const USAGE_FIELDS: [&str; 4] = [
    "input_tokens",
    "output_tokens",
    "cache_read_input_tokens",
    "cache_creation_input_tokens",
];

/// Parameters for one real Claude Code job execution.
pub struct JobSpec<'a> {
    pub workdir: &'a Path,
    pub prompt: &'a str,
    pub model: Option<&'a str>,
    pub effort: Option<&'a str>,
    pub context: &'a Context,
    /// The most the run may spend, in micro-dollars, as the engine's
    /// settings cap it: a `claude-api` run on a metered credit gets no more
    /// than the credit has room for either.
    pub max_budget_usd: Option<i64>,
    /// The credit that admits a `claude-api` run, when one is metered.
    pub credit: Option<&'a super::CreditConfig>,
}

/// Run Claude Code for `spec`, returning a structured [`JobOutcome`].
pub fn execute(
    config: &PluginConfig,
    grants: &command::Grants,
    spec: &JobSpec<'_>,
    env: &dyn Fn(&str) -> Option<String>,
    now: i64,
) -> JobOutcome {
    execute_with_stream(config, grants, spec, env, now, &mut |_| {})
}

/// [`execute`], forwarding each complete stdout line from Claude Code to
/// `on_line` in real time as it arrives.
#[expect(clippy::too_many_lines, reason = "legacy; split by job")]
pub fn execute_with_stream(
    config: &PluginConfig,
    grants: &command::Grants,
    spec: &JobSpec<'_>,
    env: &dyn Fn(&str) -> Option<String>,
    now: i64,
    on_line: &mut dyn FnMut(&str),
) -> JobOutcome {
    if !spec.workdir.is_dir() {
        return JobOutcome::Failed {
            session: None,
            error: format!("workdir {} does not exist", spec.workdir.display()),
            usage: JobUsage::default(),
        };
    }

    let (resume_session, fork_session) = match spec.context {
        Context::Fresh => (None, false),
        Context::Continue { session } => (Some(session.clone()), false),
        Context::Fork { session } => (Some(session.clone()), true),
    };

    // Read login/API credential strictly from the engine's configured
    // `credentialEnv` (or `api_key_file` fallback for `claude-api`). Never
    // embed credentials in `grants.env` (which goes into `--settings` argv).
    let cred_var = config.credential_env();
    let cred_value = env(cred_var).filter(|v| !v.trim().is_empty());

    if config.mode == Mode::Claude && !config.host_mode && cred_value.is_none() {
        return JobOutcome::Refused {
            session: None,
            refusal: Refusal {
                kind: RefusalKind::LoginExpired,
                reason: format!("no login credential is set in {cred_var}"),
                resets_at: None,
            },
            usage: JobUsage::default(),
        };
    }

    // Admitted by the credit, when one is metered, only now that nothing
    // but starting it is left: its reservation, recorded in the meter's
    // ledger, is taken back below if it never starts.
    let ledger_stream = config
        .host_mode
        .then(|| env("PROJ_CREDIT_LEDGER_FILE"))
        .flatten()
        .filter(|s| !s.is_empty());
    let reservation = match spec.credit {
        Some(credit) if config.mode == Mode::ClaudeApi => {
            let reserved = match ledger_stream.as_deref() {
                Some(path) => api::reserve_in_stream(
                    Some(&credit.credit_file),
                    &credit.jobs_dir,
                    Path::new(path),
                    now,
                    spec.max_budget_usd,
                    spec.model,
                ),
                None => api::reserve(
                    Some(&credit.credit_file),
                    &credit.jobs_dir,
                    now,
                    spec.max_budget_usd,
                ),
            };
            match reserved {
                Ok(r) => Some(r),
                Err(refused) => {
                    return JobOutcome::Refused {
                        session: None,
                        refusal: Refusal {
                            kind: RefusalKind::CreditExhausted,
                            reason: refused.to_string(),
                            resets_at: None,
                        },
                        usage: JobUsage::default(),
                    };
                }
            }
        }
        _ => None,
    };
    let budget = match &reservation {
        Some(r) => Some(r.budget),
        None => spec.max_budget_usd,
    };
    let append_system_prompt = {
        let dynamic = config
            .host_mode
            .then(|| env("PROJ_APPEND_SYSTEM_PROMPT"))
            .flatten()
            .filter(|s| !s.is_empty());
        match (&config.append_system_prompt, dynamic) {
            (Some(base), Some(extra)) => Some(format!("{base}\n\n{extra}")),
            (Some(base), None) => Some(base.clone()),
            (None, Some(extra)) => Some(extra),
            (None, None) => None,
        }
    };
    let mut extra_args = command::run_args(&command::RunArgs {
        model: spec.model.map(str::to_owned),
        effort: spec.effort.map(str::to_owned),
        session: resume_session.clone(),
        max_budget_usd: budget,
        append_system_prompt,
    });
    if fork_session {
        extra_args.push("--fork-session".to_owned());
    }

    let (mut cmd, key_guard) = match command::build_command_for(
        config.program(),
        grants,
        &extra_args,
        config.mode,
        cred_value.as_deref(),
        config.api_key_file.as_deref(),
    ) {
        Ok(built) => built,
        Err(refused) => {
            return unstarted(
                reservation,
                JobOutcome::Refused {
                    session: None,
                    refusal: Refusal {
                        kind: RefusalKind::ApiKeyRejected,
                        reason: refused.to_string(),
                        resets_at: None,
                    },
                    usage: JobUsage::default(),
                },
            );
        }
    };

    if config.mode == Mode::Claude
        && let Some(token) = cred_value
    {
        cmd.env("CLAUDE_CODE_OAUTH_TOKEN", token.trim());
    }
    if let Some(base_url) = &config.api_base_url {
        cmd.env("ANTHROPIC_BASE_URL", base_url);
    }
    cmd.current_dir(spec.workdir);

    // Register cancellation signals before spawning so no signal between spawn
    // and wait can be missed.
    let mut signals = match Signals::new([SIGTERM, SIGINT, SIGHUP]) {
        Ok(s) => s,
        Err(e) => {
            return unstarted(
                reservation,
                JobOutcome::Failed {
                    session: None,
                    error: format!("failed to register signal handlers: {e}"),
                    usage: JobUsage::default(),
                },
            );
        }
    };

    // Process-group isolation strategy:
    // - When our caller (e.g. Conary's runner) already placed this plugin as
    //   the leader of its own process group (`getpgid(0) == getpid()`), keep
    //   the harness in the same process group so a `SIGKILL` to `-plugin_pid`
    //   kills the plugin, harness, and every descendant atomically; on SIGTERM
    //   or exit we kill all remaining siblings in our process group.
    // - Otherwise (e.g. invoked from a shell or test runner's process group),
    //   place the harness in its own process group (`process_group(0)`) so
    //   killing its group never affects the caller.
    let self_pid = getpid();
    let shared_group = getpgid(None).is_ok_and(|pgid| pgid == self_pid);
    if !shared_group {
        cmd.process_group(0);
    }

    let mut child = match cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            drop(key_guard);
            return unstarted(
                reservation,
                JobOutcome::Failed {
                    session: None,
                    error: format!("failed to start {}: {e}", config.program().display()),
                    usage: JobUsage::default(),
                },
            );
        }
    };
    // The child inherited the key descriptor across exec; drop our copy now.
    drop(key_guard);

    let mut out = Vec::new();
    let run_result = drive_child(
        &mut child,
        spec.prompt.as_bytes(),
        &mut signals,
        &mut out,
        on_line,
    );
    kill_harness_group(&mut child, shared_group, self_pid);
    // Its cost, as far as its stream got, into the ledger the credit's
    // meter reads, however the run ended. One that can't be written stays
    // held at its budget there, the side that never overspends.
    if let Some(reservation) = reservation {
        let _ = reservation.finish(&out);
    }

    match run_result {
        Err(DriverError::Cancelled) => JobOutcome::Failed {
            session: None,
            error: "job cancelled by caller".to_owned(),
            usage: JobUsage::default(),
        },
        Err(DriverError::Io(why)) => JobOutcome::Failed {
            session: None,
            error: why,
            usage: JobUsage::default(),
        },
        Ok(finished) => classify_output(
            config.mode,
            spec.context,
            &out,
            &finished.stderr,
            finished.status,
            now,
        ),
    }
}

/// A run that never started gives its reservation back, and says so in its
/// outcome when it can't.
fn unstarted(reservation: Option<api::Reservation>, outcome: JobOutcome) -> JobOutcome {
    let Some(Err(held)) = reservation.map(api::Reservation::abandon) else {
        return outcome;
    };
    match outcome {
        JobOutcome::Failed {
            session,
            error,
            usage,
        } => JobOutcome::Failed {
            session,
            error: format!("{error}; {held}"),
            usage,
        },
        JobOutcome::Refused {
            session,
            mut refusal,
            usage,
        } => {
            refusal.reason = format!("{}; {held}", refusal.reason);
            JobOutcome::Refused {
                session,
                refusal,
                usage,
            }
        }
        ok @ JobOutcome::Ok { .. } => ok,
    }
}

struct Finished {
    status: ExitStatus,
    stderr: Vec<u8>,
}

enum DriverError {
    Cancelled,
    Io(String),
}

fn nonblocking(fd: &impl std::os::fd::AsFd) -> Result<(), DriverError> {
    let flags = fcntl_getfl(fd).map_err(|e| DriverError::Io(e.to_string()))?;
    fcntl_setfl(fd, flags | OFlags::NONBLOCK).map_err(|e| DriverError::Io(e.to_string()))
}

fn read_chunk(
    pipe: &mut impl Read,
    bytes: &mut Vec<u8>,
    max: usize,
    label: &str,
) -> Result<bool, DriverError> {
    let mut buf = [0u8; 8192];
    match pipe.read(&mut buf) {
        Ok(0) => Ok(true),
        Ok(n) => {
            if bytes.len() + n > max {
                return Err(DriverError::Io(format!(
                    "{label} exceeded maximum size of {max} bytes"
                )));
            }
            bytes.extend_from_slice(&buf[..n]);
            Ok(false)
        }
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) =>
        {
            Ok(false)
        }
        Err(e) => Err(DriverError::Io(e.to_string())),
    }
}

fn drive_child(
    child: &mut Child,
    prompt: &[u8],
    signals: &mut Signals,
    out: &mut Vec<u8>,
    on_line: &mut dyn FnMut(&str),
) -> Result<Finished, DriverError> {
    let mut stdin = child.stdin.take();
    if let Some(pipe) = &stdin {
        nonblocking(pipe)?;
    }
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| DriverError::Io("missing child stdout".to_owned()))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| DriverError::Io("missing child stderr".to_owned()))?;
    nonblocking(&stdout)?;
    nonblocking(&stderr)?;

    let mut remaining = prompt;
    let mut err = Vec::new();
    let mut out_done = false;
    let mut err_done = false;
    let mut emitted = 0usize;
    let mut status: Option<(ExitStatus, Instant)> = None;

    let flush_lines = |buf: &[u8], emitted: &mut usize, on_line: &mut dyn FnMut(&str)| {
        while let Some(rel) = buf[*emitted..].iter().position(|&b| b == b'\n') {
            let line = String::from_utf8_lossy(&buf[*emitted..*emitted + rel]);
            on_line(line.trim_end_matches('\r'));
            *emitted += rel + 1;
        }
    };

    loop {
        if signals.pending().next().is_some() {
            return Err(DriverError::Cancelled);
        }

        if let Some(pipe) = &mut stdin {
            if !remaining.is_empty() {
                match pipe.write(remaining) {
                    Ok(n) => remaining = &remaining[n..],
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                        ) => {}
                    Err(e) if e.kind() == io::ErrorKind::BrokenPipe => {
                        stdin = None;
                    }
                    Err(e) => return Err(DriverError::Io(e.to_string())),
                }
            }
            if remaining.is_empty() {
                stdin = None;
            }
        }

        if !out_done {
            out_done = read_chunk(&mut stdout, out, MAX_STDOUT_BYTES, "stdout")?;
            flush_lines(out, &mut emitted, on_line);
        }
        if !err_done {
            err_done = read_chunk(&mut stderr, &mut err, MAX_STDERR_BYTES, "stderr")?;
        }

        if status.is_none()
            && let Some(exited) = child
                .try_wait()
                .map_err(|e| DriverError::Io(e.to_string()))?
        {
            status = Some((exited, Instant::now()));
        }

        // Once the direct harness process has exited, finish draining any
        // buffered output; do not hang forever if a background descendant
        // inherited stdout/stderr and keeps the pipe open.
        if let Some((exited, exited_at)) = status
            && ((out_done && err_done) || exited_at.elapsed() >= Duration::from_millis(100))
        {
            if !out_done {
                let _ = read_chunk(&mut stdout, out, MAX_STDOUT_BYTES, "stdout")?;
            }
            flush_lines(out, &mut emitted, on_line);
            if emitted < out.len() {
                let tail = String::from_utf8_lossy(&out[emitted..]);
                if !tail.is_empty() {
                    on_line(tail.trim_end_matches('\r'));
                }
            }
            if !err_done {
                let _ = read_chunk(&mut stderr, &mut err, MAX_STDERR_BYTES, "stderr")?;
            }
            return Ok(Finished {
                status: exited,
                stderr: err,
            });
        }

        sleep(POLL_INTERVAL);
    }
}

/// Kill the harness's entire process group and wait briefly until no member
/// remains alive.
fn kill_harness_group(child: &mut Child, shared_group: bool, self_pid: Pid) {
    let deadline = Instant::now() + CLEANUP_TIMEOUT;
    if !shared_group {
        let group = Pid::from_child(child);
        let _ = kill_process_group(group, Signal::KILL);
        let _ = child.kill();
        let _ = child.wait();
        while test_kill_process_group(group).is_ok() && Instant::now() < deadline {
            let _ = kill_process_group(group, Signal::KILL);
            sleep(Duration::from_millis(10));
        }
    } else {
        let _ = child.kill();
        let _ = child.wait();
        let self_raw = self_pid.as_raw_nonzero().get();
        loop {
            let siblings = group_siblings(self_raw, self_raw);
            if siblings.is_empty() {
                break;
            }
            for pid in siblings {
                if let Some(p) = Pid::from_raw(pid) {
                    let _ = kill_process(p, Signal::KILL);
                }
            }
            if Instant::now() >= deadline {
                break;
            }
            sleep(Duration::from_millis(10));
        }
    }
}

/// PIDs in process group `pgid` other than `exclude_pid`, read from `/proc`.
fn group_siblings(pgid: i32, exclude_pid: i32) -> Vec<i32> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else {
            continue;
        };
        if pid == exclude_pid {
            continue;
        }
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        if proc_stat_pgid(&stat) == Some(pgid) {
            found.push(pid);
        }
    }
    found
}

/// Parse the process group ID from a Linux `/proc/<pid>/stat` line.
fn proc_stat_pgid(stat: &str) -> Option<i32> {
    let (_, rest) = stat.rsplit_once(')')?;
    let mut fields = rest.split_ascii_whitespace();
    let _state = fields.next()?;
    let _ppid = fields.next()?;
    fields.next()?.parse().ok()
}

/// Parse stream JSONL records and classify the run into [`JobOutcome`].
pub fn classify_output(
    mode: Mode,
    context: &Context,
    stdout: &[u8],
    stderr: &[u8],
    status: ExitStatus,
    now: i64,
) -> JobOutcome {
    let text = String::from_utf8_lossy(stdout);
    let mut session: Option<String> = None;
    let mut records: Vec<Value> = Vec::new();
    let mut results: Vec<String> = Vec::new();
    let mut last_result: Option<Value> = None;

    for line in text.lines() {
        if session.is_none()
            && let Some(id) = stream::session_id(line)
        {
            session = Some(id);
        }
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            records.push(v);
        }
        if let Some(res) = stream::result_of(line) {
            if let Some(piece) = res["result"].as_str().filter(|s| !s.is_empty()) {
                results.push(piece.to_owned());
            }
            last_result = Some(res);
        }
    }

    let usage = extract_usage(&records);

    let Some(last) = last_result else {
        return JobOutcome::Failed {
            session,
            error: format_exit_error("claude produced no terminal result record", status, stderr),
            usage,
        };
    };

    if let Some(refusal) = classify_refusal(mode, &last, &records, now) {
        return JobOutcome::Refused {
            session,
            refusal,
            usage,
        };
    }

    if stream::is_error(&last) {
        let message = error_text(&last).unwrap_or_else(|| {
            format_exit_error("claude returned an error result", status, stderr)
        });
        return JobOutcome::Failed {
            session,
            error: message,
            usage,
        };
    }

    if !status.success() {
        return JobOutcome::Failed {
            session,
            error: format_exit_error("claude exited unsuccessfully", status, stderr),
            usage,
        };
    }

    let final_session = match context {
        Context::Continue { session: expected } => match session {
            None => Some(expected.clone()),
            Some(ref got) if got == expected => session,
            Some(got) => {
                return JobOutcome::Failed {
                    session: Some(got.clone()),
                    // The session it did resume is the outcome's own; the
                    // error names neither, as no error carries a session.
                    error: "continuation resumed another session than the one requested".to_owned(),
                    usage,
                };
            }
        },
        Context::Fresh | Context::Fork { .. } => session,
    };

    let answer = results.join("\n\n");
    if answer.trim().is_empty() {
        return JobOutcome::Failed {
            session: final_session,
            error: "claude completed with an empty answer".to_owned(),
            usage,
        };
    }

    JobOutcome::Ok {
        session: final_session,
        answer,
        usage,
    }
}

fn classify_refusal(mode: Mode, last: &Value, records: &[Value], now: i64) -> Option<Refusal> {
    match mode {
        Mode::Claude => {
            let (resets_at, reason) = claude::refused_records(last, records.iter().cloned(), now)?;
            let kind = if reason == claude::LOGGED_OUT {
                RefusalKind::LoginExpired
            } else {
                RefusalKind::UsageLimit
            };
            Some(Refusal {
                kind,
                reason,
                resets_at,
            })
        }
        Mode::ClaudeApi => {
            let refused = api::refusal(last, records.iter().cloned())?;
            let kind = if last["subtype"] == "error_max_budget_usd" {
                RefusalKind::BudgetExhausted
            } else if refused.0.contains("didn't take its key") {
                RefusalKind::ApiKeyRejected
            } else {
                RefusalKind::CreditExhausted
            };
            Some(Refusal {
                kind,
                reason: refused.to_string(),
                resets_at: None,
            })
        }
    }
}

fn error_text(last: &Value) -> Option<String> {
    if let Some(text) = last["result"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Some(text.to_owned());
    }
    if let Some(arr) = last["errors"].as_array() {
        let joined: Vec<&str> = arr
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        if !joined.is_empty() {
            return Some(joined.join("; "));
        }
    }
    None
}

fn format_exit_error(prefix: &str, status: ExitStatus, stderr: &[u8]) -> String {
    let how = match (status.code(), status.signal()) {
        (Some(code), _) => format!("exit {code}"),
        (None, Some(sig)) => format!("signal {sig}"),
        (None, None) => "unknown status".to_owned(),
    };
    let tail = String::from_utf8_lossy(stderr);
    let snippet = tail.trim();
    if snippet.is_empty() {
        format!("{prefix} ({how})")
    } else {
        let bounded: String = snippet.chars().take(400).collect();
        format!("{prefix} ({how}): {bounded}")
    }
}

/// Extract token counts and total cost from one run's stream JSONL records.
pub fn extract_usage(records: &[Value]) -> JobUsage {
    let mut assistant_messages: BTreeMap<String, [u64; 4]> = BTreeMap::new();
    let mut totals = [0u64; 4];
    let mut saw_result_usage = false;
    let mut total_cost_usd: Option<f64> = None;

    for v in records {
        match v["type"].as_str().unwrap_or_default() {
            "assistant" => {
                if let Some(id) = v["message"]["id"].as_str() {
                    let entry = assistant_messages.entry(id.to_owned()).or_insert([0; 4]);
                    for (old, new) in entry
                        .iter_mut()
                        .zip(USAGE_FIELDS.map(|k| v["message"]["usage"][k].as_u64().unwrap_or(0)))
                    {
                        *old = (*old).max(new);
                    }
                }
            }
            "result" => {
                let tokens = USAGE_FIELDS.map(|k| v["usage"][k].as_u64().unwrap_or(0));
                let execution_crash = v["subtype"] == "error_during_execution";
                if v["usage"].is_object() && !(execution_crash && tokens == [0; 4]) {
                    saw_result_usage = true;
                    for (sum, val) in totals.iter_mut().zip(tokens) {
                        *sum += val;
                    }
                }
                if let Some(cost) = v["total_cost_usd"].as_f64()
                    && cost.is_finite()
                    && cost >= 0.0
                    && !(execution_crash && cost == 0.0)
                {
                    total_cost_usd = Some(cost);
                }
            }
            _ => {}
        }
    }

    if !saw_result_usage {
        for tokens in assistant_messages.into_values() {
            totals[0] += tokens[0];
            // Assistant output tokens are placeholders until result arrives.
            totals[2] += tokens[2];
            totals[3] += tokens[3];
        }
    }

    JobUsage {
        input_tokens: totals[0],
        output_tokens: totals[1],
        cache_read_input_tokens: totals[2],
        cache_creation_input_tokens: totals[3],
        total_cost_usd,
    }
}
