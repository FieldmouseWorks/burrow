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
use std::process::Command;

use super::{Described, PluginRefusal, RefusalKind};
use crate::config::Config;
use crate::engine::command::{host_instructions, sandbox_instructions};
use crate::job::Job;
use crate::record::{JobFile, TurnFile};
use crate::scratch;

/// Whether `engine` may run where the turn is, checked before anything of
/// it runs; a refusal when it can't, which the turn's route takes on (it
/// holds nothing, so the turn ends refused unless the route now runs it
/// elsewhere).
pub(super) fn placed(
    agent: Option<&str>,
    sandbox_engine: Option<&str>,
    engine: &str,
    credit: bool,
    described: &Described,
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
        // An engine that declares it needs a sandbox (it may run with its
        // own permissions bypassed, trusting the VM as the boundary) never
        // runs on the host.
        None if described.needs_sandbox() => refused(
            RefusalKind::Misplaced,
            format!(
                "{engine} needs a sandbox, which a turn on the host doesn't give it, so it doesn't run there"
            ),
        ),
        _ => Ok(()),
    }
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
