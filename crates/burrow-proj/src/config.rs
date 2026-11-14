//! proj's config, as modules/proj.nix writes it: PROJ_CONFIG names the file.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

use crate::engine::Engine;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub settings: Option<burrow_settings::Config>,
    #[serde(default)]
    pub policy_unit: Option<String>,
    #[serde(default)]
    pub proofs_state: Option<PathBuf>,
    #[serde(default)]
    pub agent_tool: Option<PathBuf>,
    #[serde(default)]
    pub proof_run: Option<PathBuf>,
    #[serde(default)]
    pub proof_build_driver: Vec<String>,
    /// The steward config the build driver reads now; a restored proof uses
    /// it, never the one its earlier unit was started with.
    #[serde(default)]
    pub proof_driver_config: Option<PathBuf>,
    #[serde(default)]
    pub usage_paid: bool,
    /// Where the projects are: each folder in it is one (burrow.projectsDir).
    pub projects: PathBuf,
    /// Each job's prompts, streams and answers, a folder a job.
    pub jobs: PathBuf,
    /// Host-owned question state; not a guest writable root.
    #[serde(default)]
    pub host_state: Option<PathBuf>,
    /// Where each job's scratch goes, under jobs/ (burrow.scratchDir).
    pub scratch: PathBuf,
    /// The slice every turn runs in, which the machine caps.
    pub slice: String,
    /// How many turns run Claude at once, how many of those slots are the
    /// pipeline's own, and the most one project's other jobs hold.
    pub max_jobs: usize,
    /// Claude concurrency while turns queued before a usage reset drain.
    #[serde(default = "usage_trickle")]
    pub usage_trickle: usize,
    pub pipeline_slots: usize,
    pub max_jobs_per_project: usize,
    /// MiB to spare before another turn starts while one runs.
    pub min_free_memory: u64,
    /// The most memory one turn may hold (systemd's MemoryMax).
    pub job_memory_max: String,
    /// A turn's timeout, as timeout(1) reads one, unless its job has its own.
    pub timeout: String,
    /// Seconds a queued turn waits before it counts as --first.
    pub max_wait: u64,
    /// How long a turn may wait in line before the stall watchdog takes it.
    pub stall_after_minutes: u64,
    /// How long an await waits on a PR or issue that doesn't change.
    pub await_hours: u64,
    /// The directory of the snapshot change stream's subscriber sockets.
    #[serde(default)]
    pub stream: Option<PathBuf>,
    #[serde(default = "ask_timeout")]
    pub ask_timeout: u64,
    #[serde(default)]
    pub probe: Option<burrow_probe::Config>,
    /// Only the crash acknowledgement socket, never host units state.
    #[serde(default)]
    pub units_clear_socket: Option<PathBuf>,
    #[serde(default = "waiting_owner_label")]
    pub waiting_owner_label: String,
    #[serde(default = "needs_owner_label")]
    pub needs_owner_label: String,
    /// burrow-events' kick port: `http://127.0.0.1:PORT/kick`.
    pub kick: String,
    /// proj-watch's wake file, where it books its next look at each repo.
    pub wake: PathBuf,
    /// What every job waits on, for the stall watchdog; None when it's off.
    pub waits: Option<PathBuf>,
    /// What marks proj-inbox's own comments: never news to a job.
    pub inbox_mark: String,
    /// The pipeline's own repos, as OWNER/NAME in any case: pipelineRepos
    /// and leadRepos, and the lead ones among them.
    pub pipeline_repos: Vec<String>,
    pub lead_repos: Vec<String>,
    /// The machine's own flake, which is the pipeline's wherever its origin is.
    pub flake: PathBuf,
    /// The proj the machine has deployed, which a waiting turn follows.
    pub deployed: PathBuf,
    /// Each GitHub App's env file, by its name.
    pub apps: BTreeMap<String, PathBuf>,
    /// The machine's own GitHub App (burrow.github.machinery), one of
    /// `apps`: what a bot's issue's job, and every Codex turn with no App
    /// of its own, acts as ([`crate::turn::github_app`]). None for none.
    pub machinery: Option<String>,
    /// Identity of cfg.tools.gh, independently of the engine's selected App.
    #[serde(default)]
    pub host_github_app: Option<String>,
    /// Host token service observations; never a job-writable directory.
    #[serde(default)]
    pub github_meters: Option<PathBuf>,
    /// The steward App's account, distinct from the App running jobs.
    #[serde(default)]
    pub steward_account: Option<burrow_github::Author>,
    /// The machinery App's account: its issues are sorted and run like an
    /// author's, admitted by this id alone. Its comments stay unheard.
    #[serde(default)]
    pub machinery_account: Option<burrow_github::Author>,
    /// The owner's notifier (burrow.tools.notify), when there is one.
    pub notify: Option<PathBuf>,
    /// Where the old roles, models and roles.d files were (~/.config/proj):
    /// read once by `proj settings migrate` into the settings overlay, which
    /// holds each role's engine, model and effort since.
    #[serde(default)]
    pub old_config: Option<PathBuf>,
    /// The file with the flake's value of each option in burrow-settings'
    /// schema, and whether the flake set it (modules/settings/tracked.nix),
    /// for `proj settings` ([`crate::settings`]). A path the system puts in
    /// /etc, not a store path: those values may run proj themselves.
    #[serde(default)]
    pub tracked_settings: Option<PathBuf>,
    /// The engines' meters (claude.json, codex.json), written by
    /// usage-reset; absent disables overflow.
    pub meters: Option<PathBuf>,
    /// Codex's own pool, and the immutable hooks dir it gives git.
    pub codex_slots: usize,
    pub codex_hooks: PathBuf,
    /// The machine's guard, as the jobs' Claude Code's PreToolUse hook,
    /// where nothing else installs it (burrow.claude off): None where the
    /// machine's managed settings already run it for every Claude.
    #[serde(default)]
    pub claude_guard: Option<ClaudeGuard>,
    /// Root-owned heavy slot directory, read-only in Codex jobs; None when CPU sharing is off.
    pub heavy_dir: Option<PathBuf>,
    /// How many slots heavy has there (burrow.cpu.heavy.slots).
    #[serde(default)]
    pub heavy_slots: u32,
    /// Machine MCP servers, shared by every job engine.
    #[serde(default)]
    pub mcp_servers: BTreeMap<String, McpServer>,
    pub tools: Tools,
    /// proj-inbox's settings; None when it's off.
    pub inbox: Option<Inbox>,
    /// The file it was read from, which each turn's unit is given too.
    #[serde(skip)]
    pub path: PathBuf,
    /// The proj this is, whole: PROJ_SELF, its wrapper, as a store path.
    #[serde(skip)]
    pub me: PathBuf,
}

/// A PreToolUse hook's command, and how long Claude Code gives it.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClaudeGuard {
    pub command: PathBuf,
    pub timeout: u64,
}

/// A stdio MCP server and the host paths its tools need.
#[derive(Clone, Debug, serde::Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpServer {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub env_vars: Vec<String>,
    #[serde(default)]
    pub read_paths: Vec<PathBuf>,
    #[serde(default)]
    pub write_paths: Vec<PathBuf>,
}

/// The programs it runs.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Tools {
    pub codex: PathBuf,
    /// Starts a Codex job's Codex over the job's own /tmp.
    pub bwrap: PathBuf,
    pub bash: PathBuf,
    pub flock: PathBuf,
    pub gh: PathBuf,
    pub git: PathBuf,
    pub systemctl: PathBuf,
    pub systemd_run: PathBuf,
    pub timeout: PathBuf,
}

/// proj-inbox's settings: GitHub issues as proj jobs.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Inbox {
    #[serde(default)]
    pub planning: bool,
    /// What it keeps of each issue it has a job for, a folder an issue.
    pub state: PathBuf,
    /// Each label selects its engine; Codex wins when both are present.
    pub labels: BTreeMap<String, Engine>,
    /// Machine issue labels mapped to routing roles.
    #[serde(default)]
    pub roles: BTreeMap<String, String>,
    /// The role an authors' own issue runs as when no machine rule names one;
    /// its engine label still picks the engine. None keeps label routing alone.
    #[serde(default)]
    pub owner_role: Option<String>,
    /// The label that, with an engine label, makes an issue's job one that waits in no
    /// line (proj run --unstick): the steward's stuck issue's.
    pub unstick_label: String,
    /// Whose issues and replies it acts on.
    pub authors: Vec<burrow_github::Author>,
    /// Its jobs run in agent sandboxes (proj run --vm).
    pub vm: bool,
    /// The machine's name, as its comments sign it.
    pub host: String,
    /// Where it books its next look at each repo.
    pub wake: PathBuf,
    /// The label on an issue carrying text from outside the authors, whoever
    /// filed it: never a job, never sorted or handed on. "" is none.
    #[serde(default)]
    pub outside_label: String,
    /// Sorting every other open issue, and handing the ready ones to idle
    /// slots; None when it's off.
    #[serde(default)]
    pub triage: Option<Triage>,
}

/// proj-inbox's triage and drain ([`crate::inbox`]'s triage and drain).
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Triage {
    #[serde(default = "sort_unlabelled")]
    pub sort_unlabelled: bool,
    /// The label on an issue only the owner can settle (the steward's owner
    /// label, which lists PRs for them).
    pub owner_label: String,
    /// Who such an issue is assigned to, so GitHub tells them; None assigns
    /// nobody.
    pub owner: Option<String>,
    /// Labels whose issues other machinery owns (probe, undo, flaky): left
    /// alone, never sorted or handed on.
    pub skip: Vec<String>,
    /// Sorting jobs at once, across every repo.
    pub at_once: usize,
    /// The engine ready issues are handed to, by its inbox label; None hands
    /// none on.
    pub drain: Option<Engine>,
    /// Handed-on issues' jobs at once, across every repo.
    pub drain_jobs: usize,
}

fn sort_unlabelled() -> bool {
    true
}

impl Config {
    pub fn paused(&self, repo: &str) -> bool {
        self.settings
            .as_ref()
            .is_none_or(|s| !s.allows(repo).unwrap_or(false))
    }

    pub fn retry_at(&self, repo: &str) -> Option<i64> {
        self.settings.as_ref()?.decision(repo).ok()?.retry_at
    }

    /// The GitHub repository of each project folder.
    pub fn project_repos(&self) -> Vec<String> {
        std::fs::read_dir(&self.projects)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter(|e| e.path().is_dir())
            .map(|e| crate::pick::github_repo(self, &e.path().to_string_lossy()))
            .filter(|r| !r.is_empty())
            .collect()
    }

    /// The projects' repositories whose automated work is paused.
    pub fn paused_repos(&self) -> Vec<String> {
        let mut repos: Vec<_> = self
            .project_repos()
            .into_iter()
            .filter(|r| self.paused(r))
            .collect();
        repos.sort();
        repos
    }

    pub fn settings_line(&self) -> String {
        let Some(settings) = &self.settings else {
            return "Automated work: paused (settings are not configured)\n".into();
        };
        let repos = self.project_repos();
        settings
            .show_for(&repos)
            .unwrap_or_else(|e| format!("Automated work: paused (settings error: {e})\n"))
    }

    /// The config PROJ_CONFIG names.
    pub fn load() -> Result<Config, String> {
        let path = std::env::var_os("PROJ_CONFIG").ok_or("PROJ_CONFIG isn't set")?;
        let shown = path.to_string_lossy().into_owned();
        let text = std::fs::read(&path).map_err(|e| format!("{shown}: {e}"))?;
        let cfg: Config = serde_json::from_slice(&text).map_err(|e| format!("{shown}: {e}"))?;
        let me = std::env::var_os("PROJ_SELF").ok_or("PROJ_SELF isn't set")?;
        let me =
            std::fs::canonicalize(&me).map_err(|e| format!("{}: {e}", me.to_string_lossy()))?;
        Ok(Config {
            path: path.into(),
            me,
            ..cfg
        })
    }

    /// The env file of the GitHub App NAME: None for one this machine hasn't.
    pub fn app_env(&self, name: &str) -> Option<&PathBuf> {
        let ok = !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        ok.then(|| self.apps.get(name)).flatten()
    }

    /// An explicit host-configured resource, not a pathname supplied by a job.
    pub(crate) fn app_environment(&self, name: &str) -> Option<String> {
        // NixOS installs this configured capability through an /etc symlink.
        // Only a key in the host's immutable app map selects it; job state
        // and all paths derived from job state use the confined reader.
        self.app_env(name)
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|text| text.trim_end_matches('\n').to_owned())
    }

    /// The role settings, one version of them, read again for every turn.
    /// One that can't be read, or names what its engine doesn't offer,
    /// fails the turn; so does reading them while proj's old files are
    /// still to be moved in, rather than running the defaults.
    pub fn role_view(&self) -> Result<burrow_settings::roles::View, String> {
        let Some(settings) = &self.settings else {
            return Ok(burrow_settings::roles::View {
                in_force: burrow_settings::roles::Roles::default().in_force(&BTreeMap::new())?,
                set: (false, false, false),
                setups: None,
                migrated: false,
                file: self.jobs.join(".settings-unconfigured"),
            });
        };
        let view = settings.role_view()?;
        if !view.migrated
            && let Some(dir) = &self.old_config
            && let Some(old) = crate::settings::old_files(dir)?
        {
            return Err(format!(
                "{}: proj's old role settings aren't moved into the settings yet (proj settings migrate)",
                old.display()
            ));
        }
        Ok(view)
    }

    /// The role settings in force.
    pub fn role_settings(&self) -> Result<burrow_settings::roles::InForce, String> {
        Ok(self.role_view()?.in_force)
    }

    /// The engine a turn of ROLE in REPO runs on: the closest role key's,
    /// or the engine it moves to when that one is short by its meter.
    pub fn engine(&self, role: &str, repo: Option<&str>) -> Result<Engine, String> {
        self.engine_in(&self.role_settings()?, role, repo)
    }

    /// The same, from settings already read.
    pub(crate) fn engine_in(
        &self,
        settings: &burrow_settings::roles::InForce,
        role: &str,
        repo: Option<&str>,
    ) -> Result<Engine, String> {
        crate::engine::route(
            settings.route(role, repo),
            self.meters.as_deref(),
            crate::usage::out(self).is_some(),
            crate::now(),
        )
    }

    /// Validate the turn-start configuration without starting a job: the
    /// module's JSON (Config::load) and the role settings every turn reads.
    /// Runtime state (meters, usage holds and individual jobs) is not
    /// machine configuration. The sandbox controller, when agents are on,
    /// must be a program proj can run: pause and release call it.
    pub fn check_config(&self) -> Result<(), String> {
        if let Some(tool) = &self.agent_tool {
            burrow_process::executable(tool)
                .map_err(|e| format!("agentTool {}: {e}", tool.display()))?;
        }
        self.role_view().map(|_| ())
    }

    /// How long a turn may wait in line, in seconds.
    pub fn stall_queued(&self) -> i64 {
        (self.stall_after_minutes * 60) as i64
    }
}

fn ask_timeout() -> u64 {
    6 * 3600
}

fn usage_trickle() -> usize {
    2
}

fn waiting_owner_label() -> String {
    "waiting: owner".into()
}
fn needs_owner_label() -> String {
    "needs-owner".into()
}

#[cfg(test)]
pub(crate) mod tests {
    use serde_json::{Value, json};

    use super::{Config, Engine};

    /// A config as the module writes one, with host event publication disabled.
    /// Tests that exercise publication must supply their own file and receiver.
    pub(crate) fn example() -> Value {
        json!({
            "settings": {"overlay":"", "allowed":["sam/box","sam/burrow","sam/game","sam/web"], "pipelineRepos":[]},
            "projects": "/data/dev/src",
            "jobs": "/data/dev/jobs",
            "scratch": "/data/dev/scratch",
            "slice": "claude-jobs.slice",
            "maxJobs": 4,
            "pipelineSlots": 1,
            "maxJobsPerProject": 2,
            "minFreeMemory": 4096,
            "jobMemoryMax": "8G",
            "timeout": "4h",
            "maxWait": 1800,
            "stallAfterMinutes": 120,
            "awaitHours": 24,
            "kick": "",
            "wake": "/var/lib/burrow/proj-watch/wake.json",
            "waits": null,
            "inboxMark": "<!-- proj-inbox -->",
            "pipelineRepos": ["sam/burrow", "sam/box"],
            "leadRepos": ["sam/burrow"],
            "flake": "/data/dev/src/box",
            "deployed": "/nowhere/proj",
            "apps": {"workers": "/run/workers.env"},
            "machinery": "workers",
            "notify": "/bin/notify",
            "meters": null, "codexSlots": 2, "codexHooks": "/hooks",
            "inbox": {"state": "/var/lib/burrow/proj-inbox", "labels": {"claude": "claude", "codex": "codex"}, "unstickLabel": "stuck",
                      "authors": [burrow_github::fake::author("sam")], "vm": false, "host": "box",
                      "wake": "/var/lib/burrow/proj-inbox/wake.json"},
            "tools": {"codex": "/bin/codex", "bwrap": "/bin/bwrap", "bash": "/bin/bash", "flock": "/bin/flock", "gh": "/bin/gh", "git": "/bin/git", "systemctl": "/bin/systemctl",
                      "systemdRun": "/bin/systemd-run", "timeout": "/bin/timeout"}
        })
    }

    #[test]
    fn test_configuration_cannot_publish_to_the_host_watchdog() {
        let cfg: Config = serde_json::from_value(example()).unwrap();
        assert!(
            cfg.waits.is_none(),
            "tests must opt into a temporary watch file"
        );
        assert!(
            cfg.kick.is_empty(),
            "tests must opt into their own event receiver"
        );
    }

    /// Points CFG's settings at an overlay in DIR, and makes each change
    /// in LINES through the settings door, as `proj settings` does.
    pub(crate) fn set(cfg: &mut Config, dir: &std::path::Path, lines: &[&str]) {
        let settings = cfg.settings.get_or_insert_with(Default::default);
        if settings.overlay.as_os_str().is_empty() {
            settings.overlay = dir.join("overlay.json");
        }
        for line in lines {
            let args: Vec<String> = line.split_whitespace().map(str::to_owned).collect();
            settings
                .command(&args, "test")
                .unwrap_or_else(|e| panic!("{line}: {e}"));
        }
    }

    /// Sets each `ROLE ENGINE [over PCT ENGINE]` line of TEXT as a route,
    /// through the door, as [`set`] does.
    pub(crate) fn routes(cfg: &mut Config, dir: &std::path::Path, text: &str) {
        let lines: Vec<String> = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| format!("role {l}"))
            .collect();
        set(
            cfg,
            dir,
            &lines.iter().map(String::as_str).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn check_config_reads_the_role_settings_every_turn_reads() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg: Config = serde_json::from_value(example()).unwrap();
        cfg.check_config().unwrap(); // Nothing set: the declared defaults.
        set(&mut cfg, dir.path(), &["role unused codex over 80 claude"]);
        crate::cmd::dispatch(&cfg, &["check-config".into()]).unwrap();
        // An overlay a hand edit broke fails it, naming why.
        let overlay = &cfg.settings.as_ref().unwrap().roles_file();
        let mut edited: Value = serde_json::from_slice(&std::fs::read(overlay).unwrap()).unwrap();
        edited["routing"]["models"]["unused claude"] = json!({"model": "gone", "effort": null});
        std::fs::write(overlay, edited.to_string()).unwrap();
        let err = crate::cmd::dispatch(&cfg, &["check-config".into()]).unwrap_err();
        assert_eq!(err.code, 1);
        let said = err.say.unwrap();
        assert!(said.contains("doesn't offer that model"), "{said}");
        // An overlay that can't be read is never "the defaults".
        std::fs::remove_file(overlay).unwrap();
        std::fs::create_dir(overlay).unwrap();
        assert!(cfg.check_config().is_err());
    }

    #[test]
    fn check_config_requires_a_runnable_agent_tool() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg: Config = serde_json::from_value(example()).unwrap();
        cfg.check_config().unwrap(); // No agents, no controller to check.
        // The package's bin/agent that was never built (#711).
        let tool = dir.path().join("agent");
        cfg.agent_tool = Some(tool.clone());
        let err = cfg.check_config().unwrap_err();
        assert!(
            err.contains(&format!("agentTool {}", tool.display())),
            "{err}"
        );
        std::fs::write(&tool, "#!/bin/sh\n").unwrap();
        assert!(
            cfg.check_config()
                .unwrap_err()
                .contains("not an executable file")
        );
        burrow_test::executable(&tool, "#!/bin/sh\n");
        cfg.check_config().unwrap();
        cfg.agent_tool = Some(dir.path().to_owned());
        assert!(
            cfg.check_config()
                .unwrap_err()
                .contains("not an executable file")
        );
    }

    #[test]
    fn the_closest_role_key_picks_the_engine() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg: Config = serde_json::from_value(example()).unwrap();
        assert_eq!(cfg.engine("routine", None).unwrap(), Engine::Claude);
        assert_eq!(cfg.engine("planner", None).unwrap(), Engine::Codex);
        set(
            &mut cfg,
            dir.path(),
            &[
                "role routine codex",
                "role work@Sam/Code codex",
                "role work claude over 80 codex",
            ],
        );
        assert_eq!(cfg.engine("routine", None).unwrap(), Engine::Codex);
        assert_eq!(cfg.engine("missing", None).unwrap(), Engine::Claude);
        assert_eq!(cfg.engine("work", Some("sAM/cODE")).unwrap(), Engine::Codex);
        for repo in [Some("sam/other"), Some("other/code"), None] {
            assert_eq!(cfg.engine("work", repo).unwrap(), Engine::Claude);
        }
        assert_eq!(
            cfg.engine("missing", Some("sam/code")).unwrap(),
            Engine::Claude
        );
    }

    // The module's config reads whole; a field the binary doesn't know, or
    // one it's missing, fails it.
    #[test]
    fn config() {
        let cfg: Config = serde_json::from_value(example()).unwrap();
        assert_eq!(cfg.stall_queued(), 7200);
        assert_eq!(
            cfg.app_env("workers").and_then(|p| p.to_str()),
            Some("/run/workers.env")
        );
        assert_eq!(cfg.app_env("nosuch"), None);
        assert_eq!(cfg.app_env("../workers"), None);
        assert_eq!(cfg.app_env(""), None);
        let mut off = example();
        off["waits"] = Value::Null;
        assert!(
            serde_json::from_value::<Config>(off)
                .unwrap()
                .waits
                .is_none()
        );
        assert_eq!(
            cfg.inbox
                .as_ref()
                .and_then(|i| i.labels.get("codex"))
                .copied(),
            Some(Engine::Codex)
        );
        let mut off = example();
        off["inbox"] = Value::Null;
        assert!(
            serde_json::from_value::<Config>(off)
                .unwrap()
                .inbox
                .is_none()
        );
        let mut odd = example();
        odd["inbox"]["labels"] = "x".into();
        assert!(serde_json::from_value::<Config>(odd).is_err());
        let mut odd = example();
        odd["jobs2"] = "/x".into();
        assert!(serde_json::from_value::<Config>(odd).is_err());
        let mut odd = example();
        odd["tools"]["curl"] = "/bin/curl".into();
        assert!(serde_json::from_value::<Config>(odd).is_err());
        let mut short = example();
        short.as_object_mut().unwrap().remove("kick");
        assert!(serde_json::from_value::<Config>(short).is_err());
    }
}
