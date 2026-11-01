//! What the autoland steward keeps up in each repo besides its PRs: its
//! labels and settings (settle.rs) and its webhook (hook.rs), the branches
//! nothing needs any more (branches.rs), Renovate's dashboard (dashboard.rs)
//! and the flaky tests' quarantine (flaky.rs); and for the reviewers, each
//! one's look at a PR queued (queue.rs) with its brief (brief.rs) on the
//! snapshot it reads (snapshot.rs, from the steward's mirror of the repo,
//! mirror.rs), their answers, posted on its PRs (post.rs) and read back
//! as their verdicts (verdict.rs), and what they leave, swept once nothing
//! needs it (sweep.rs); and for its Claude jobs that settle a PR (claims.rs),
//! their answers, posted only once the PR's head is their push (answer.rs). A
//! pass of the steward's runs
//! `settle`, then `branches` once it has seen to the repo's PRs, then
//! `watch`, on each repo it sees to, with the App's token for the repo's
//! owner in GH_TOKEN, which `token` (token.rs) mints the pass. A full pass
//! ends with `hooks`, which mints its own, for each owner it's about. The
//! pass acts on what `branches` and `watch` say (pass::Out): it lists
//! what's stuck, books the repo's next pass, and starts the Claude jobs
//! `watch` asks for, each under its key. Nothing is told back: a part's
//! next look finds its job by that key (`proj find`).

pub mod answer;
pub mod branches;
pub mod brief;
pub mod claims;
pub mod config;
pub mod dashboard;
pub mod flaky;
pub mod hook;
pub mod mirror;
pub mod pass;
pub mod post;
pub mod queue;
pub mod settle;
pub mod snapshot;
pub mod sweep;
#[cfg(test)]
mod testing;
pub mod token;
pub mod verdict;

use pass::{Pass, repo_file};

/// The repo's labels and settings, then its hook, which is due when they
/// were. A status run leaves the hook alone: it has no `secret` (only the
/// services are given it), and nothing to change. One that can't be read
/// fails the look; GitHub not giving or taking the hook is only logged, and
/// it's tried again next pass.
pub fn settle(p: &mut Pass, repo: &settle::Repo, secret: impl FnOnce() -> Result<String, String>) {
    let cfg = p.cfg;
    let due = settle::settle(p, &cfg.settle, repo);
    if p.dry {
        return;
    }
    let secret = match secret() {
        Ok(s) => s,
        Err(e) => {
            p.out.failed.push(format!("its webhook: {e}"));
            return;
        }
    };
    let mut st: hook::State = p.load(&repo_file(p.repo));
    if let Err(e) = hook::keep(p, &secret, &mut st, due) {
        eprintln!("{}: its webhook: {e}", p.repo);
    }
}

/// GitHub's time, in seconds.
pub fn ts(at: &str) -> Result<f64, String> {
    let t: jiff::Timestamp = at
        .parse()
        .map_err(|e| format!("GitHub's time {at:?}: {e}"))?;
    Ok(t.as_millisecond() as f64 / 1000.0)
}

/// Each part the config has on, each on its own: one that fails says so in
/// `failed`, and the rest go on. A part that fails asks for no job: what it
/// asked for may not be kept (its state wasn't saved), and its next look
/// couldn't find the job by its key, and would start another beside it.
pub fn watch(p: &mut Pass) {
    let cfg = p.cfg;
    if let Some(d) = &cfg.dashboard {
        let asked = p.out.jobs.len();
        if let Err(e) = dashboard::watch(p, d) {
            p.out.jobs.truncate(asked);
            p.out.failed.push(format!("its dashboard: {e}"));
        }
    }
    if let Some(f) = &cfg.flaky {
        let asked = p.out.jobs.len();
        if let Err(e) = flaky::watch(p, f) {
            p.out.jobs.truncate(asked);
            p.out.failed.push(format!("its flaky tests: {e}"));
        }
    }
}
