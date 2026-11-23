//! What modules/updates.nix tells renovate-sweep.

use std::path::PathBuf;

use serde::Deserialize;

/// What modules/updates.nix tells renovate-sweep.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Config {
    /// renovate-sweep, as the machine names it: how it says itself.
    pub name: String,
    /// Its own: what each PR was last handed over for (OWNER_REPO-N), its
    /// refresh requests taken back (OWNER_REPO-N.released), and the key
    /// it seals verdicts on majors with (verdict.key, seal.rs).
    pub state: PathBuf,
    /// Where the machine's projects are: each clone of a GitHub repo, a
    /// project its job runs in.
    pub projects: PathBuf,
    /// Who decides a major that's held, as the task names them.
    pub owner: String,
    /// The autoland steward's own commit status, which isn't CI.
    pub settled: String,
    /// The repos the steward covers, as it last found them (its
    /// repos.json): none without it.
    pub covered: Option<PathBuf>,
    /// burrow-landing's record (burrow.landing.record), read at each pass: a
    /// major in a repo it gives conaryd gets its verdicts sealed as conaryd
    /// reads them (v2), the rest as the steward does (v1). None where
    /// nothing hands landing over (no autoland): every repo the steward's.
    pub landing: Option<PathBuf>,
    /// The machine's own App, when the jobs act as it.
    pub app: Option<App>,
    /// Who Renovate comments as, as REST spells it: its own App's bot
    /// (burrow.updates.renovate.app), which needn't be the jobs'. None:
    /// Renovate runs as the user, whose gh login the sweep's is too.
    pub renovate: Option<String>,
    /// The longest a Renovate run goes on, in seconds: a refresh request
    /// taken back waits that long before a job may push.
    pub renovate_run: u64,
    /// The labels that keep the steward off a PR (burrow.autoland.labels'
    /// hold and owner): a PR with one is left alone, handed to no job and
    /// asked of no Renovate run, until it comes off.
    pub held: Vec<String>,
    pub tools: Tools,
}

/// The App the jobs act on GitHub as.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct App {
    /// As proj run --app names it.
    pub name: String,
    /// Its bot's login as GraphQL spells it: who the jobs post as.
    pub bot: String,
}

/// What it runs, by path.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Tools {
    /// gh, as the App when there's one.
    pub gh: PathBuf,
    pub git: PathBuf,
    pub proj: PathBuf,
    /// HOST-notify, which tells the owner.
    pub notify: PathBuf,
    /// lock-wait (modules/autoland/lock_wait.py): when a lock file refresh that
    /// lock-age refused for age only can go again, and how far the sweep
    /// goes asking.
    pub lock_wait: PathBuf,
}
