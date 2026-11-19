//! burrow-probe: read-only questions about the machine, asked on a private
//! repo's issues and answered there in seconds, with no Claude. An open issue
//! labelled `probe` (the config's `label`) in the config's repo, opened by
//! one of its authors, is a list of questions, one a line, in a fixed
//! vocabulary (query.rs); burrow-events runs a pass on the repo's issue
//! events, which answers each with one comment and closes it. Reopening it
//! asks again.
//!
//! What keeps it read-only and keeps secrets in: no shell, and nothing of the
//! issue's text reaches a command but a unit name, executable path or time
//! query.rs made; unit journals only of the units on its allowlist, and crash
//! reports only of an exact executable with selected diagnostic fields; a repo that isn't private
//! gets no answer at all; and every answer goes through the scrubber
//! (scrub.rs) before it leaves. `burrow-probe CONFIG ask QUESTION` answers
//! one here, on the terminal, as it is: whoever runs it can read the same
//! things anyway.

pub mod answer;
pub mod pass;
pub mod query;
pub mod scrub;

use std::path::PathBuf;

use serde::Deserialize;

/// The probe's config, as modules/probe.nix writes it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// OWNER/NAME: the private repo whose issues it answers.
    pub repo: String,
    /// OWNER/NAME of the machine's own flake, when it is on GitHub: the repo
    /// whose commits `contains` checks against the configuration revision.
    /// Independent of `repo`, which is only where the issues live.
    #[serde(default)]
    pub system_repo: Option<String>,
    /// The label that makes an issue a probe.
    pub label: String,
    /// Who may ask (GitHub logins, any case).
    pub authors: Vec<burrow_github::Author>,
    /// The configured machinery App, using the same read-only door.
    #[serde(default)]
    pub machinery: Option<burrow_github::Author>,
    /// The machine's name, for the answer's first line.
    pub host: String,
    /// The user it runs as, whose user manager `units`, `rc` and `queue`
    /// read.
    pub user: String,
    /// The units whose journals it may read: globs (`*`, `?`, `[...]`).
    pub journal: Journal,
    /// Where HOST-update keeps live.json (burrow.stateDir).
    pub state_dir: PathBuf,
    /// proj, for `queue`; None where proj isn't on.
    pub proj: Option<PathBuf>,
    #[serde(default)]
    pub projects: Option<PathBuf>,
    #[serde(default)]
    pub jobs: Option<PathBuf>,
    pub tools: Tools,
    /// The machine's Actions runners, as its configuration defines them
    /// (`runners`).
    #[serde(default)]
    pub runners: Vec<Runner>,
    /// Who the deployed configuration trusts, and what its jobs act as
    /// (`trust`, and the marks `issue` puts on writers).
    #[serde(default)]
    pub trust: Option<Trust>,
    /// Path regexes the steward's reviewers never read
    /// (`autoland.reviewers.skipPaths` and each repo's): no answer opens,
    /// lists or resolves a path one matches, nor one a link points to.
    #[serde(default)]
    pub hidden: Vec<String>,
}

/// One runner, or the template of the runners started fresh for one job
/// each, as the machine's configuration has its unit: configured facts,
/// which `runners` sets beside what the loaded unit and the files say.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Runner {
    /// The unit, without `.service`: `burrow-fresh-runner@` for the template.
    pub name: String,
    pub kind: RunnerKind,
    /// The repos whose jobs it takes.
    pub repos: Vec<String>,
    pub labels: Vec<String>,
    /// Whether this system's configuration defines the unit at all.
    pub defined: bool,
    /// Its `User=`, if it names one.
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub dynamic_user: bool,
    /// The home of the unit's user, which it gets when its environment sets
    /// no HOME (a configured HOME is in `environment`).
    #[serde(default)]
    pub home: Option<String>,
    /// The packages the unit's `path` puts on PATH.
    #[serde(default)]
    pub packages: Vec<String>,
    /// The unit's configured environment, only the keys PATH, HOME,
    /// RUSTUP_HOME, RUSTUP_TOOLCHAIN and CARGO_HOME.
    #[serde(default)]
    pub environment: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RunnerKind {
    /// One of the machine's own (burrow.dispatch.runners).
    Named,
    /// burrow's runners started fresh for one job (burrow.dispatch.fresh).
    Fresh,
}

/// The deployed trust lists and GitHub identities.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Trust {
    /// proj-inbox, when it's on.
    #[serde(default)]
    pub inbox: Option<Inbox>,
    /// The engines door's authors, when it's on.
    #[serde(default)]
    pub engines: Option<Vec<burrow_github::Author>>,
    /// The machine's GitHub Apps, by name.
    #[serde(default)]
    pub apps: Vec<App>,
    /// burrow.github.machinery: one of `apps`, by name.
    #[serde(default)]
    pub machinery: Option<String>,
    /// The steward's deployed config (/etc/burrow/autoland.json), when
    /// autoland is on.
    #[serde(default)]
    pub autoland: Option<PathBuf>,
    /// Whether agent sandboxes are on.
    #[serde(default)]
    pub sandboxes: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Inbox {
    pub authors: Vec<burrow_github::Author>,
    /// Issue jobs run in agent sandboxes.
    pub vm: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct App {
    pub name: String,
    pub login: String,
    pub id: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Journal {
    /// System units.
    pub units: Vec<String>,
    /// The user's own units (`journalctl --user-unit`).
    pub user_units: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Tools {
    #[serde(default = "git")]
    pub git: PathBuf,
    pub journalctl: PathBuf,
    pub systemctl: PathBuf,
    /// The gh the probe asks GitHub with (`issue`): the machine's own,
    /// burrow.github.machineGh. None: gh from PATH.
    #[serde(default)]
    pub gh: Option<PathBuf>,
}

/// The mark on the probe's own comments.
pub const MARK: &str = "<!-- burrow-probe -->";

fn git() -> PathBuf {
    "git".into()
}
