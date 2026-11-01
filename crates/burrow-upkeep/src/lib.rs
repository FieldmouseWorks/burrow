//! What the autoland steward keeps up in each repo besides its PRs:
//! Renovate's dashboard (dashboard.rs) and the flaky tests' quarantine
//! (flaky.rs). A pass of the steward's runs `watch` on each repo it sees to,
//! with the App's token for the repo's owner in GH_TOKEN, and acts on what
//! it says (pass::Out): it lists what's stuck, books the repo's next pass,
//! and starts the Claude jobs it asks for, telling each back (`started`).

pub mod config;
pub mod dashboard;
pub mod flaky;
pub mod pass;
#[cfg(test)]
mod testing;

use pass::Pass;

/// Each part the config has on, each on its own: one that fails says so in
/// `failed`, and the rest go on. A part that fails asks for no job: what it
/// asked for may not be kept (its state wasn't saved), and a job nothing
/// kept as asked for could start beside one never recorded.
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

/// What the steward tells back of a job a part asked for: it started, as
/// `Some(jid)`, or it didn't, or was stopped. By the part's name, as the
/// job's request gives it.
pub fn told(p: &mut Pass, part: &str, jid: Option<&str>) -> Result<(), String> {
    match (part, jid) {
        ("dashboard", Some(jid)) => dashboard::started(p, jid),
        ("dashboard", None) => dashboard::unstarted(p),
        ("flaky", Some(jid)) => flaky::started(p, jid),
        ("flaky", None) => flaky::unstarted(p),
        _ => Err(format!("no part {part:?}: dashboard or flaky")),
    }
}
