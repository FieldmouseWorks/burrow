//! Where a turn's plugin runs, given the `timeout(1)` command [`super::spawn`]
//! started building.
//!
//! On the host, in a login shell and then the job's App env, where
//! `proj _plugin` resolves the job's host grants. In a `--vm` job's own
//! sandbox, over `agent sh`, as the engine ran there before plugins: the
//! sandbox shares the host's store, so the configured plugin runs there as
//! it is. Only there does the guest's `proj` write the turn's declaration
//! where [`crate::guest::collect`] reads it (`PROJ_VM_NEXT`), does a
//! continued turn find its session, and does the work stay for
//! `agent fetch`.

use std::io::{self, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use burrow_grants::{Approved, Capability, Grants, Why, mcp};
use conary_core::plugin::Kind;
use rustix::process::{Pid, Signal, kill_process_group, test_kill_process_group};

use super::{Described, MAX_RESPONSE_BYTES, PluginRefusal, RefusalKind};
use crate::config::Config;
use crate::engine::command::{host_instructions, sandbox_instructions};
use crate::job::Job;
use crate::record::{JobFile, TurnFile};
use crate::scratch;

/// Capabilities a `proj --vm` job VM (`agent new`) unconditionally exposes to
/// a plugin running inside it over `agent sh`. A sandbox plugin whose owner
/// omitted any of these from `burrow.agents.engines.NAME.capabilities` must
/// not launch in a job VM.
pub(super) const VM_EXPOSED_CAPABILITIES: &[Capability] = &[
    Capability::Workspace,
    Capability::JobState,
    Capability::GitMetadata,
    Capability::Toolchains,
    Capability::System,
    Capability::Probe,
    Capability::UnitsClear,
    Capability::Heavy,
    Capability::Nix,
    Capability::Network,
];

/// Maximum wall-clock timeout for a host `describe` call.
const DESCRIBE_TIMEOUT_SECS: &str = "30s";
/// Maximum wall-clock timeout for a sandboxed `describe` call (allows a stopped
/// microVM to boot over `agent sh`).
const SANDBOX_DESCRIBE_TIMEOUT_SECS: &str = "180s";

/// Why [`super::spawn`] started nothing.
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

/// Placement and pre-launch grant checks that can refuse a turn **before**
/// executing the plugin at all (even for `describe`): a sandbox made for
/// another engine, a credit engine inside a sandbox, a sandbox-only engine on
/// the host, or a sandbox-only engine whose owner-approved capabilities omit
/// what a `proj --vm` job VM exposes.
pub(super) fn before_describe(
    agent: Option<&str>,
    sandbox_engine: Option<&str>,
    engine: &str,
    credit: bool,
    sandbox_approved: Option<&[String]>,
) -> Result<(), PluginRefusal> {
    let refused = |kind, reason: String| {
        Err(PluginRefusal {
            kind,
            reason,
            resets_at: None,
        })
    };
    match agent {
        // A sandbox has the login and hosts of the engine it was made for
        // alone: another engine couldn't sign in or reach its service there.
        Some(_)
            if let Some(made) = sandbox_engine
                && made != engine =>
        {
            refused(
                RefusalKind::Misplaced,
                format!(
                    "{engine} isn't the engine the job's sandbox was made for ({made}), so it doesn't run there"
                ),
            )
        }
        // A credit is admitted and spent through its ledger on the host,
        // which a sandbox's root guest is never given.
        Some(_) if credit => refused(
            RefusalKind::CreditExhausted,
            format!(
                "{engine} runs on a credit, which a sandbox can't be admitted to, so it doesn't run there"
            ),
        ),
        // A job VM attaches workspace, toolchains, probe, nix and filtered
        // egress: refuse a sandbox plugin before launch if the owner withheld
        // any of those capabilities in `burrow.agents.engines.NAME.capabilities`.
        Some(_) if let Some(approved) = sandbox_approved => {
            let missing: Vec<&str> = VM_EXPOSED_CAPABILITIES
                .iter()
                .map(|c| c.name())
                .filter(|name| !approved.iter().any(|a| a == name))
                .collect();
            if !missing.is_empty() {
                return refused(
                    RefusalKind::Misplaced,
                    format!(
                        "{engine} is missing owner-approved capabilities for a job VM ({}): refusing launch",
                        missing.join(", ")
                    ),
                );
            }
            Ok(())
        }
        // A plugin wired from `burrow.agents.engines` may only run inside its
        // sandbox: refuse it on the host before ever launching its executable.
        None if sandbox_approved.is_some() => refused(
            RefusalKind::Misplaced,
            format!(
                "{engine} needs a sandbox, which a turn on the host doesn't give it, so it doesn't run there"
            ),
        ),
        _ => Ok(()),
    }
}

/// Place `cmd` in a new process group of its own (`pgid == child.id()`) so
/// [`reap_group`] can terminate any lingering descendants that inherited its
/// stdout/stderr pipes after the wrapper exits.
pub(super) fn isolate_group(cmd: &mut Command) {
    cmd.process_group(0);
}

/// Terminate any surviving descendants in process group `pgid` after the
/// leader has exited, so pipe reader threads never block indefinitely on an
/// inherited descriptor. Returns immediately when no process remains.
pub(super) fn reap_group(pgid: u32) {
    let Some(pid) = i32::try_from(pgid).ok().and_then(Pid::from_raw) else {
        return;
    };
    if test_kill_process_group(pid).is_err() {
        return;
    }
    let _ = kill_process_group(pid, Signal::TERM);
    for _ in 0..20 {
        if test_kill_process_group(pid).is_err() {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let _ = kill_process_group(pid, Signal::KILL);
}

/// Run `describe` on `plugin_path` (inside `agent` over `agent sh` for a
/// sandboxed turn, or on the host for a host turn), draining stdout/stderr
/// concurrently and reaping any lingering descendants in its process group
/// before joining readers.
pub(super) fn run_describe(
    cfg: &Config,
    plugin_path: &Path,
    engine: &str,
    agent: Option<&str>,
    req_bytes: &[u8],
) -> Result<Vec<u8>, String> {
    let mut cmd = describe_command(cfg, plugin_path, agent);
    let mut child = cmd.spawn().map_err(|e| {
        format!(
            "couldn't start {engine} plugin {}: {e}",
            plugin_path.display()
        )
    })?;
    let pgid = child.id();
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin
            .write_all(req_bytes)
            .and_then(|()| stdin.write_all(b"\n"));
    }
    let stdout_handle = child.stdout.take().map(|out| {
        thread::spawn(move || {
            let mut buf = Vec::new();
            out.take(MAX_RESPONSE_BYTES as u64 + 1)
                .read_to_end(&mut buf)?;
            Ok::<_, io::Error>(buf)
        })
    });
    let stderr_handle = child.stderr.take().map(|mut err| {
        thread::spawn(move || {
            let mut kept = Vec::new();
            let _ = (&mut err).take(64 * 1024).read_to_end(&mut kept);
            let _ = io::copy(&mut err, &mut io::sink());
            kept
        })
    });
    let status = child
        .wait()
        .map_err(|e| format!("{engine} plugin describe wait failed: {e}"))?;
    reap_group(pgid);
    let stdout = match stdout_handle {
        Some(h) => h
            .join()
            .map_err(|_| format!("{engine} plugin describe stdout reader panicked"))?
            .map_err(|e| format!("{engine} plugin describe stdout read failed: {e}"))?,
        None => Vec::new(),
    };
    let stderr = stderr_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();
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
    Ok(stdout)
}

/// Build the `timeout(1)` command for `describe`: inside `agent` over
/// `agent sh` for a sandboxed turn (so sandbox plugins never execute on the
/// host), or directly on the host with a scrubbed environment for a host turn.
fn describe_command(cfg: &Config, plugin_path: &Path, agent: Option<&str>) -> Command {
    let mut cmd = Command::new(&cfg.tools.timeout);
    let timeout = if agent.is_some() {
        SANDBOX_DESCRIBE_TIMEOUT_SECS
    } else {
        DESCRIBE_TIMEOUT_SECS
    };
    cmd.args(["-k", "10s", timeout]);
    if let Some(agent) = agent {
        cmd.args(["agent", "sh", agent]);
    } else {
        // Offline `describe` on the host never needs the service user's
        // tokens, home directory, or working tree.
        cmd.env_clear().current_dir("/");
        if let Some(path) = std::env::var_os("PATH") {
            cmd.env("PATH", path);
        }
    }
    cmd.arg(plugin_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    isolate_group(&mut cmd);
    cmd
}

/// Whether `engine` may run where the turn is, checked after `describe` as
/// well; enforces `burrow-grants` capability intersection for sandbox plugins
/// in a `--vm` turn and refuses host turns on any plugin declaring
/// `Sandbox::Needs`.
pub(super) fn placed(
    agent: Option<&str>,
    sandbox_engine: Option<&str>,
    engine: &str,
    credit: bool,
    sandbox_approved: Option<&[String]>,
    described: &Described,
) -> Result<(), PluginRefusal> {
    before_describe(agent, sandbox_engine, engine, credit, sandbox_approved)?;
    if agent.is_none() && described.needs_sandbox() {
        return Err(PluginRefusal {
            kind: RefusalKind::Misplaced,
            reason: format!(
                "{engine} needs a sandbox, which a turn on the host doesn't give it, so it doesn't run there"
            ),
            resets_at: None,
        });
    }
    if agent.is_some()
        && let Some(approved_caps) = sandbox_approved
    {
        let registry = mcp::Registry::default();
        let approved = Approved {
            capabilities: approved_caps,
            mcp_servers: &[],
            registry: &registry,
        };
        let grants = Grants::new(
            &[Kind::Job],
            &described.negotiated.offers.capabilities,
            &approved,
        );
        let denied: Vec<String> = grants
            .refusals()
            .iter()
            .filter(|r| matches!(r.why, Why::NotApproved | Why::Never(_) | Why::UnknownDevice))
            .map(|r| r.capability.clone())
            .collect();
        if !denied.is_empty() {
            return Err(PluginRefusal {
                kind: RefusalKind::Misplaced,
                reason: format!(
                    "{engine} declared capabilities not approved for its sandbox ({}): refusing launch",
                    denied.join(", ")
                ),
                resets_at: None,
            });
        }
    }
    Ok(())
}

/// Run `plugin` inside the job's sandbox `agent`, with what the turn tells
/// the guest: where its `proj` records the declaration (a stale one from an
/// earlier run of this turn removed first), the turn's instructions, and
/// that the plugin is to stream its events. Nothing else of the host's
/// environment goes in.
pub(super) fn sandboxed(
    cmd: &mut Command,
    job: &Job,
    n: &str,
    agent: &str,
    must: bool,
    plugin: &Path,
) {
    cmd.args(["agent", "sh", agent, "env"])
        .arg(format!("PROJ_VM_NEXT={}", crate::guest::path(job, n)))
        .arg(format!(
            "PROJ_APPEND_SYSTEM_PROMPT={}",
            sandbox_instructions(must)
        ))
        .arg("PROJ_STREAM_EVENTS=1")
        .args(["sh", "-c", r#"rm -f -- "$PROJ_VM_NEXT"; exec "$@""#, "sh"])
        .arg(plugin);
}

/// Run `plugin` on the host. Inside a login shell followed by any configured
/// App env, so nothing in the login profile can undo `--app`; then `_plugin`
/// resolves host capability grants in that final environment and execs the
/// configured plugin executable.
#[allow(clippy::too_many_arguments)]
pub(super) fn host(
    cmd: &mut Command,
    cfg: &Config,
    job: &Job,
    n: &str,
    waitdir: &Path,
    must: bool,
    app_env: &str,
    plugin: &Path,
) {
    cmd.arg(&cfg.tools.bash)
        .args([
            "-lc",
            r#"eval "$PROJ_APP_ENV" && unset PROJ_APP_ENV && exec "$@""#,
        ])
        .arg("proj-engine")
        .arg(&cfg.me)
        .arg("_plugin")
        .arg(plugin)
        .env("PROJ_PROBE_SOCKET", crate::probe::socket(cfg, job, n))
        .env(
            "PROJ_HOST_JOBS_SOCKET",
            crate::host_jobs::socket(cfg, job, n),
        )
        .env("PROJ_JOB", job.id)
        .env("PROJ_GITHUB_DIR", job.file(JobFile::Github))
        .env(
            "PROJ_GITHUB_KICK",
            if cfg.kick.is_empty() {
                String::new()
            } else {
                format!("{}/proj-watch", cfg.kick)
            },
        )
        .env(
            "PROJ_GITHUB_REFUSALS",
            job.turn_file(n, TurnFile::GithubRefusals),
        )
        .env("PROJ_WAIT_DIR", waitdir)
        .env("PROJ_SCRATCH", scratch::dir(cfg, job.id))
        .env("PROJ_APP_ENV", app_env)
        .env(
            "PROJ_APPEND_SYSTEM_PROMPT",
            host_instructions(cfg, job, must),
        );
}
