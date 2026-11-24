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

use std::path::Path;
use std::process::{Command, Stdio};

use super::{Described, PluginRefusal, RefusalKind};
use crate::config::Config;
use crate::engine::command::{host_instructions, sandbox_instructions};
use crate::job::Job;
use crate::record::{JobFile, TurnFile};
use crate::scratch;

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

/// Placement checks that can refuse a turn **before** executing the plugin at
/// all (even for `describe`): a sandbox made for another engine, a credit
/// engine inside a sandbox, or a sandbox-only engine on the host.
pub(super) fn before_describe(
    agent: Option<&str>,
    sandbox_engine: Option<&str>,
    engine: &str,
    credit: bool,
    sandbox_only: bool,
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
        // A plugin wired from `burrow.agents.engines` may only run inside its
        // sandbox: refuse it on the host before ever launching its executable.
        None if sandbox_only => refused(
            RefusalKind::Misplaced,
            format!(
                "{engine} needs a sandbox, which a turn on the host doesn't give it, so it doesn't run there"
            ),
        ),
        _ => Ok(()),
    }
}

/// Build the `timeout(1)` command for `describe`: inside `agent` over
/// `agent sh` for a sandboxed turn (so sandbox plugins never execute on the
/// host), or directly on the host for a host turn.
pub(super) fn describe_command(cfg: &Config, plugin_path: &Path, agent: Option<&str>) -> Command {
    let mut cmd = Command::new(&cfg.tools.timeout);
    let timeout = if agent.is_some() {
        SANDBOX_DESCRIBE_TIMEOUT_SECS
    } else {
        DESCRIBE_TIMEOUT_SECS
    };
    cmd.args(["-k", "10s", timeout]);
    if let Some(agent) = agent {
        cmd.args(["agent", "sh", agent]);
    }
    cmd.arg(plugin_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

/// Whether `engine` may run where the turn is, checked after `describe` as
/// well; a refusal when it can't, which the turn's route takes on (it holds
/// nothing, so the turn ends refused unless the route now runs it elsewhere).
pub(super) fn placed(
    agent: Option<&str>,
    sandbox_engine: Option<&str>,
    engine: &str,
    credit: bool,
    described: &Described,
) -> Result<(), PluginRefusal> {
    before_describe(
        agent,
        sandbox_engine,
        engine,
        credit,
        described.needs_sandbox(),
    )
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
