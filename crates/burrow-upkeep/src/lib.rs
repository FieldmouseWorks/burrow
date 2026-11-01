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
/// `failed`, and the rest go on.
pub fn watch(p: &mut Pass) {
    let cfg = p.cfg;
    if let Some(d) = &cfg.dashboard
        && let Err(e) = dashboard::watch(p, d)
    {
        p.out.failed.push(format!("its dashboard: {e}"));
    }
    if let Some(f) = &cfg.flaky
        && let Err(e) = flaky::watch(p, f)
    {
        p.out.failed.push(format!("its flaky tests: {e}"));
    }
}

/// The parts that ask for jobs, by the name a job request gives.
pub fn started(p: &mut Pass, part: &str, jid: &str) -> Result<(), String> {
    match part {
        "dashboard" => dashboard::started(p, jid),
        "flaky" => flaky::started(p, jid),
        _ => Err(format!("no part {part:?}: dashboard or flaky")),
    }
}
