//! What the autoland steward keeps up in each repo besides its PRs: its
//! labels and settings (settle.rs), Renovate's dashboard (dashboard.rs) and
//! the flaky tests' quarantine (flaky.rs). A pass of the steward's runs
//! `settle` and then `watch` on each repo it sees to, with the App's token
//! for the repo's owner in GH_TOKEN. It sets the repo's hook once its
//! settings are seen to, and acts on what `watch` says (pass::Out): it lists
//! what's stuck, books the repo's next pass, and starts the Claude jobs it
//! asks for, each under its key. Nothing is told back: a part's next look
//! finds its job by that key (`proj find`).

pub mod config;
pub mod dashboard;
pub mod flaky;
pub mod pass;
pub mod settle;
#[cfg(test)]
mod testing;

use pass::Pass;

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
