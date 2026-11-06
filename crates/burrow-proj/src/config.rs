//! proj's config, as modules/proj.nix writes it: PROJ_CONFIG names the file.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

use crate::engine::{Choice, Engine};

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// Where the projects are: each folder in it is one (burrow.projectsDir).
    pub projects: PathBuf,
    /// Each job's prompts, streams and answers, a folder a job.
    pub jobs: PathBuf,
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
    /// The owner's notifier (burrow.tools.notify), when there is one.
    pub notify: Option<PathBuf>,
    /// ROLE ENGINE[:MODEL] [over PCT ENGINE[:MODEL]], outside the repos.
    /// Missing roles use Claude's default; malformed lines fail the turn.
    pub roles: PathBuf,
    /// Claude's weekly reading, written by usage-reset; absent disables overflow.
    pub meter: Option<PathBuf>,
    /// Codex's own pool, and the immutable hooks dir it gives git.
    pub codex_slots: usize,
    pub codex_hooks: PathBuf,
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
    /// The machine's own GitHub App, which a bot's issue's job acts as.
    pub machinery: Option<String>,
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

    /// Resolve one role once per turn. Every non-comment line must parse,
    /// so a typo never silently sends work to a different engine.
    pub fn engine(&self, role: &str) -> Result<Choice, String> {
        let text = match std::fs::read_to_string(&self.roles) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(format!("{}: {e}", self.roles.display())),
        };
        let mut selected = None;
        for (i, line) in text.lines().enumerate() {
            if line.trim().is_empty() || line.trim_start().starts_with('#') {
                continue;
            }
            let (name, choice) = crate::engine::role(line, self.meter.as_deref(), crate::now())
                .map_err(|e| format!("{} line {}: {line}: {e}", self.roles.display(), i + 1))?;
            if name == role {
                if selected.is_some() {
                    return Err(format!(
                        "{} line {}: duplicate role {role}",
                        self.roles.display(),
                        i + 1
                    ));
                }
                selected = Some(choice);
            }
        }
        Ok(selected.unwrap_or_default())
    }

    /// How long a turn may wait in line, in seconds.
    pub fn stall_queued(&self) -> i64 {
        (self.stall_after_minutes * 60) as i64
    }
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
            "notify": "/bin/notify",
            "roles": "/nowhere/roles",
            "meter": null, "codexSlots": 2, "codexHooks": "/hooks",
            "inbox": {"state": "/var/lib/burrow/proj-inbox", "labels": {"claude": "claude", "codex": "codex"}, "unstickLabel": "stuck",
                      "authors": ["sam"], "vm": false, "host": "box",
                      "wake": "/var/lib/burrow/proj-inbox/wake.json", "machinery": "workers"},
            "tools": {"codex": "/bin/codex", "bash": "/bin/bash", "flock": "/bin/flock", "gh": "/bin/gh", "git": "/bin/git", "systemctl": "/bin/systemctl",
                      "systemdRun": "/bin/systemd-run", "timeout": "/bin/timeout"}
        })
    }

    #[test]
    fn roles() {
        let dir = tempfile::tempdir().unwrap();
        let roles = dir.path().join("roles");
        let mut cfg: Config = serde_json::from_value(example()).unwrap();
        cfg.roles = roles.clone();
        assert_eq!(cfg.engine("routine").unwrap(), Choice::default());
        std::fs::write(&roles, "# comment\nroutine codex:chosen\njudge claude\n").unwrap();
        assert_eq!(
            cfg.engine("routine").unwrap(),
            Choice {
                engine: Engine::Codex,
                model: Some("chosen".into())
            }
        );
        assert_eq!(cfg.engine("judge").unwrap(), Choice::default());
        assert_eq!(cfg.engine("missing").unwrap(), Choice::default());
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
        ] {
            std::fs::write(&roles, format!("# comment\n{bad}\n")).unwrap();
            let error = cfg.engine("routine").unwrap_err();
            assert!(error.contains("line 2") && error.contains(bad), "{error}");
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
