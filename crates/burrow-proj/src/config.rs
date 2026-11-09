//! proj's config, as modules/proj.nix writes it: PROJ_CONFIG names the file.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

use crate::engine::{Choice, Engine};

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
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
    #[serde(default = "ask_timeout")]
    pub ask_timeout: u64,
    #[serde(default)]
    pub probe: Option<burrow_probe::Config>,
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
    /// The owner's notifier (burrow.tools.notify), when there is one.
    pub notify: Option<PathBuf>,
    /// ROLE[@OWNER/REPO] ENGINE[:MODEL] [over PCT ENGINE[:MODEL]], outside the repos.
    /// Missing roles use Claude's default; malformed lines fail the turn.
    pub roles: PathBuf,
    /// The engines' meters (claude.json, codex.json), written by
    /// usage-reset; absent disables overflow.
    pub meters: Option<PathBuf>,
    /// Codex's own pool, and the immutable hooks dir it gives git.
    pub codex_slots: usize,
    pub codex_hooks: PathBuf,
    /// Root-owned heavy slot directory, read-only in Codex jobs; None when CPU sharing is off.
    pub heavy_dir: Option<PathBuf>,
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
    /// What it keeps of each issue it has a job for, a folder an issue.
    pub state: PathBuf,
    /// Each label selects its engine; Codex wins when both are present.
    pub labels: BTreeMap<String, Engine>,
    /// The label that, with an engine label, makes an issue's job one that waits in no
    /// line (proj run --unstick): the steward's stuck issue's.
    pub unstick_label: String,
    /// Whose issues and replies it acts on.
    pub authors: Vec<String>,
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

impl Config {
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

    /// Resolve one role once per turn. Every non-comment line must parse,
    /// so a typo never silently sends work to a different engine. Repo scopes
    /// beat bare roles; duplicate keys fail with both lines, even on other roles.
    pub fn engine(&self, role: &str, repo: Option<&str>) -> Result<Choice, String> {
        let text = crate::roles::read(&self.roles)?;
        let entries = crate::roles::parse(
            &self.roles,
            &text,
            self.meters.as_deref(),
            crate::usage::out(self).is_some(),
            crate::now(),
        )?;
        Ok(entries
            .into_iter()
            .filter_map(|(key, choice)| key.specificity(role, repo).map(|rank| (rank, choice)))
            .max_by_key(|(rank, _)| *rank)
            .map(|(_, choice)| choice)
            .unwrap_or_default())
    }

    /// Validate the turn-start configuration without starting a job. Config::load
    /// already parses the module's JSON; engine validates every external role,
    /// including unused scopes and overflow choices. Runtime state (meters,
    /// usage holds and individual jobs) is not machine configuration.
    pub fn check_config(&self) -> Result<(), String> {
        self.engine("", None).map(|_| ())
    }

    /// How long a turn may wait in line, in seconds.
    pub fn stall_queued(&self) -> i64 {
        (self.stall_after_minutes * 60) as i64
    }
}

fn ask_timeout() -> u64 {
    6 * 3600
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

    use super::{Choice, Config, Engine};

    /// A config as the module writes one, for the tests to change.
    pub(crate) fn example() -> Value {
        json!({
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
            "kick": "http://127.0.0.1:8788/kick",
            "wake": "/var/lib/burrow/proj-watch/wake.json",
            "waits": "/data/dev/jobs/.waits.json",
            "inboxMark": "<!-- proj-inbox -->",
            "pipelineRepos": ["sam/burrow", "sam/box"],
            "leadRepos": ["sam/burrow"],
            "flake": "/data/dev/src/box",
            "deployed": "/nowhere/proj",
            "apps": {"workers": "/run/workers.env"},
            "machinery": "workers",
            "notify": "/bin/notify",
            "roles": "/nowhere/roles",
            "meters": null, "codexSlots": 2, "codexHooks": "/hooks",
            "inbox": {"state": "/var/lib/burrow/proj-inbox", "labels": {"claude": "claude", "codex": "codex"}, "unstickLabel": "stuck",
                      "authors": ["sam"], "vm": false, "host": "box",
                      "wake": "/var/lib/burrow/proj-inbox/wake.json"},
            "tools": {"codex": "/bin/codex", "bwrap": "/bin/bwrap", "bash": "/bin/bash", "flock": "/bin/flock", "gh": "/bin/gh", "git": "/bin/git", "systemctl": "/bin/systemctl",
                      "systemdRun": "/bin/systemd-run", "timeout": "/bin/timeout"}
        })
    }

    #[test]
    fn check_config_rejects_legacy_roles_and_checks_unused_lines() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg: Config = serde_json::from_value(example()).unwrap();
        cfg.roles = dir.path().join("roles");
        cfg.check_config().unwrap(); // An absent file means default engines.
        std::fs::write(&cfg.roles, "# roles\nunused old-model\n").unwrap();
        let err = crate::cmd::dispatch(&cfg, &["check-config".into()]).unwrap_err();
        let said = err.say.unwrap();
        assert_eq!(err.code, 1);
        assert!(
            said.contains(&format!("{} line 2", cfg.roles.display())),
            "{said}"
        );
        assert!(said.contains("unknown engine old-model"), "{said}");
        std::fs::write(&cfg.roles, "unused claude\nother codex over 80 claude\n").unwrap();
        crate::cmd::dispatch(&cfg, &["check-config".into()]).unwrap();
        // An existing but unreadable roles path must not mean defaults.
        std::fs::remove_file(&cfg.roles).unwrap();
        std::fs::create_dir(&cfg.roles).unwrap();
        assert!(
            cfg.check_config()
                .unwrap_err()
                .contains(&cfg.roles.display().to_string())
        );
    }

    #[test]
    fn roles() {
        let dir = tempfile::tempdir().unwrap();
        let roles = dir.path().join("roles");
        let mut cfg: Config = serde_json::from_value(example()).unwrap();
        cfg.roles = roles.clone();
        assert_eq!(cfg.engine("routine", None).unwrap(), Choice::default());
        std::fs::write(&roles, "# comment\nroutine codex:chosen\njudge claude\n").unwrap();
        assert_eq!(
            cfg.engine("routine", None).unwrap(),
            Choice {
                engine: Engine::Codex,
                model: Some("chosen".into())
            }
        );
        assert_eq!(cfg.engine("judge", None).unwrap(), Choice::default());
        assert_eq!(cfg.engine("missing", None).unwrap(), Choice::default());
        for bad in [
            "bare",
            "routine old-format",
            "routine unknown",
            "routine codex:",
            "routine codex:a$b",
            "routine codex extra",
            "routine claude over 101 codex",
            "routine claude over nope codex",
            "routine claude over 50",
            "work@ codex",
            "work@owner codex",
            "work@/repo codex",
            "work@owner/ codex",
            "work@owner/repo/extra codex",
            "work@owner/repo@other codex",
            "@owner/repo codex",
        ] {
            std::fs::write(&roles, format!("# comment\n{bad}\n")).unwrap();
            let error = cfg.engine("routine", None).unwrap_err();
            assert!(error.contains("line 2") && error.contains(bad), "{error}");
        }
    }

    #[test]
    fn scoped_roles_beat_bare_roles_in_either_order_and_bare_beats_none() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg: Config = serde_json::from_value(example()).unwrap();
        cfg.roles = dir.path().join("roles");
        let scoped = "work@Sam/Code codex:scoped";
        let bare = "work claude:bare";
        for lines in [format!("{scoped}\n{bare}\n"), format!("{bare}\n{scoped}\n")] {
            std::fs::write(&cfg.roles, lines).unwrap();
            assert_eq!(
                cfg.engine("work", Some("sAM/cODE")).unwrap(),
                Choice {
                    engine: Engine::Codex,
                    model: Some("scoped".into())
                }
            );
            for repo in [Some("sam/other"), Some("other/code"), None] {
                assert_eq!(
                    cfg.engine("work", repo).unwrap(),
                    Choice {
                        engine: Engine::Claude,
                        model: Some("bare".into())
                    }
                );
            }
            assert_eq!(
                cfg.engine("missing", Some("sam/code")).unwrap(),
                Choice::default()
            );
        }
        std::fs::write(&cfg.roles, scoped).unwrap();
        assert_eq!(cfg.engine("work", None).unwrap(), Choice::default());
        assert_eq!(
            cfg.engine("work", Some("sam/other")).unwrap(),
            Choice::default()
        );
    }

    #[test]
    fn duplicate_role_keys_report_both_lines_even_for_an_unselected_role() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg: Config = serde_json::from_value(example()).unwrap();
        cfg.roles = dir.path().join("roles");
        for (first, second) in [
            ("work claude:bare", "work codex"),
            ("work@Sam/Code claude", "work@sam/code codex over 80 claude"),
        ] {
            std::fs::write(
                &cfg.roles,
                format!("# header\n{first}\n# comment\n{second}\n"),
            )
            .unwrap();
            let error = cfg.engine("other", Some("sam/other")).unwrap_err();
            assert!(
                error.contains(&format!("line 2: {first}"))
                    && error.contains(&format!("line 4: {second}")),
                "{error}"
            );
        }
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
