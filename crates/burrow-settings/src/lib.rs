//! The tracked admission policy plus one live overlay. Every decision reloads
//! the overlay; an unreadable policy fails closed. No pipeline daemon is needed
//! to change it. Other settings can join this document without another store.
pub mod door;
pub mod schema;
pub mod show;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs::{self, File},
    path::PathBuf,
};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    pub overlay: PathBuf,
    #[serde(default)]
    pub allowed: Vec<String>,
    #[serde(default)]
    pub pipeline_repos: Vec<String>,
}

/// A scheduling decision and its next change without another external event.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Decision {
    pub allowed: bool,
    pub retry_at: Option<i64>,
    pub vacate: bool,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Overlay {
    #[serde(default)]
    repos: BTreeMap<String, bool>,
    #[serde(default)]
    pipeline_until: Option<i64>,
    #[serde(default)]
    receipts: BTreeMap<String, String>,
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

const CHANGED: &str = "Settings changed; use settings show for the current policy.";
const REPOS: &str = "settings.repositoryOverrides";
const UNTIL: &str = "settings.pipelineUntil";

/// What a write to `name` lasts, as the schema declares.
fn lasts(name: &str) -> &'static str {
    match schema::find(name).map(|s| s.write) {
        Some(schema::Write::Door { lasts }) => lasts,
        _ => "not at all: the door doesn't write it",
    }
}

impl Overlay {
    /// The value the overlay holds for an option it keeps, None when unset.
    fn get(&self, name: &str) -> Option<Value> {
        match name {
            REPOS => (!self.repos.is_empty()).then(|| serde_json::json!(self.repos)),
            UNTIL => self.pipeline_until.map(|t| serde_json::json!(t)),
            _ => None,
        }
    }

    /// Set (or, with None, clear) an option the door writes; the value has
    /// already passed the schema's kind.
    fn apply(&mut self, name: &str, value: Option<&Value>) -> Result<(), String> {
        match (name, value) {
            (REPOS, None) => self.repos.clear(),
            (REPOS, Some(v)) => {
                let map: BTreeMap<String, bool> =
                    serde_json::from_value(v.clone()).map_err(|e| e.to_string())?;
                self.repos = map
                    .into_iter()
                    .map(|(k, b)| (k.to_ascii_lowercase(), b))
                    .collect();
            }
            (UNTIL, None) => self.pipeline_until = None,
            (UNTIL, Some(v)) => self.pipeline_until = v.as_i64(),
            _ => return Err(format!("{name}: the overlay keeps no such option")),
        }
        Ok(())
    }
}

const USAGE: &str = "usage: settings show | set NAME JSON because REASON | unset NAME because REASON | allow OWNER/REPO | pause OWNER/REPO | pipeline UNTIL | back";

/// One request to the door, already checked against the schema.
#[derive(Debug, PartialEq)]
pub enum Request {
    Show,
    Allow(String),
    Pause(String),
    Pipeline(i64),
    Back,
    Set {
        name: String,
        value: Value,
        reason: String,
    },
    Unset {
        name: String,
        reason: String,
    },
}

fn reason(words: &[&str]) -> Result<String, String> {
    match words {
        ["because", why @ ..] if why.len() >= 2 => Ok(why.join(" ")),
        _ => Err("say why: ... because REASON (two words or more)".into()),
    }
}

/// An option the door writes, or why it won't.
fn writable(name: &str) -> Result<&'static schema::Setting, String> {
    let setting = schema::find(name).ok_or(format!("{name}: no such option"))?;
    match setting.write {
        schema::Write::Door { .. } => Ok(setting),
        schema::Write::Refused { reason } => Err(format!("{name} is not changed here: {reason}")),
    }
}

/// Read a request, refusing it with its reason when it can't be one. Nothing
/// is changed by reading.
pub fn parse(args: &[String]) -> Result<Request, String> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["show"] => Ok(Request::Show),
        ["back"] => Ok(Request::Back),
        [verb @ ("allow" | "pause"), repo] if burrow_github::repo_name(repo) => {
            Ok(if *verb == "allow" {
                Request::Allow(repo.to_ascii_lowercase())
            } else {
                Request::Pause(repo.to_ascii_lowercase())
            })
        }
        ["pipeline", until] => {
            let t: i64 = until
                .parse()
                .map_err(|_| "pipeline needs a Unix timestamp")?;
            if t <= now() {
                return Err("pipeline deadline must be in the future".into());
            }
            Ok(Request::Pipeline(t))
        }
        ["set", name, value, why @ ..] => {
            let setting = writable(name)?;
            let value: Value = serde_json::from_str(value)
                .map_err(|e| format!("{name}: value is not JSON: {e}"))?;
            setting
                .kind
                .check(&value)
                .map_err(|e| format!("{name}: {e}"))?;
            Ok(Request::Set {
                name: setting.name.into(),
                value,
                reason: reason(why)?,
            })
        }
        ["unset", name, why @ ..] => Ok(Request::Unset {
            name: writable(name)?.name.into(),
            reason: reason(why)?,
        }),
        _ => Err(USAGE.into()),
    }
}

/// The door validates requests before attempting mutations; malformed issues
/// are answered rather than blocking later off requests.
pub fn valid_command(args: &[String]) -> bool {
    parse(args).is_ok()
}

impl Config {
    /// The operational values consumed by the typed settings read door.
    pub fn readings(&self) -> Result<(BTreeMap<String, bool>, Option<i64>), String> {
        let overlay = self.read()?;
        Ok((
            overlay.repos,
            overlay.pipeline_until.filter(|until| *until > now()),
        ))
    }

    fn read(&self) -> Result<Overlay, String> {
        match fs::read(&self.overlay) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| e.to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Overlay::default()),
            Err(e) => Err(e.to_string()),
        }
    }

    /// Read again on every admission, including a long-lived reader's next job.
    pub fn allows(&self, repo: &str) -> Result<bool, String> {
        Ok(self.decision(repo)?.allowed)
    }

    pub fn decision(&self, repo: &str) -> Result<Decision, String> {
        let state = self.read()?;
        let allowed = self.permits(&state, repo);
        Ok(Decision {
            allowed,
            vacate: state.repos.get(&repo.to_ascii_lowercase()) == Some(&false)
                || (state.pipeline_until.is_some_and(|t| t > now())
                    && !self
                        .pipeline_repos
                        .iter()
                        .any(|r| r.eq_ignore_ascii_case(repo))),
            retry_at: state.pipeline_until.filter(|t| !allowed && *t > now()),
        })
    }

    pub fn scope_until(&self) -> Result<Option<i64>, String> {
        Ok(self.read()?.pipeline_until.filter(|t| *t > now()))
    }

    fn permits(&self, state: &Overlay, repo: &str) -> bool {
        let allowed = state
            .repos
            .get(&repo.to_ascii_lowercase())
            .copied()
            .unwrap_or_else(|| self.allowed.iter().any(|r| r.eq_ignore_ascii_case(repo)));
        allowed
            && (!state.pipeline_until.is_some_and(|t| t > now())
                || self
                    .pipeline_repos
                    .iter()
                    .any(|r| r.eq_ignore_ascii_case(repo)))
    }

    pub fn show(&self) -> Result<String, String> {
        self.show_for(&[])
    }

    /// Include discovered repositories when presenting the machine's queue.
    pub fn show_for(&self, known: &[String]) -> Result<String, String> {
        let state = self.read()?;
        let scoped = state.pipeline_until.filter(|t| *t > now());
        let mut repos: BTreeMap<String, bool> = self
            .allowed
            .iter()
            .map(|r| (r.to_ascii_lowercase(), true))
            .collect();
        for repo in known {
            repos.entry(repo.to_ascii_lowercase()).or_insert(false);
        }
        repos.extend(state.repos.clone());
        let allowed: Vec<_> = repos
            .keys()
            .filter(|r| self.permits(&state, r))
            .cloned()
            .collect();
        let paused: Vec<_> = repos
            .keys()
            .filter(|r| !self.permits(&state, r))
            .cloned()
            .collect();
        Ok(format!(
            "Automated work: {}; allowed: {}; paused: {}; all unlisted repos paused (including new repos)\n{}",
            scoped.map_or("allow list".into(), |t| format!("pipeline-only until {t}")),
            allowed.join(", "),
            paused.join(", "),
            self.options(&state)
        ))
    }

    /// A line for each option the policy keeps: its value, the layer it
    /// comes from (default, flake or overlay) and the last change logged.
    fn options(&self, state: &Overlay) -> String {
        let live = |name: &str| {
            state
                .get(name)
                .filter(|v| name != UNTIL || v.as_i64().is_some_and(|t| t > now()))
        };
        let mut out = String::new();
        for s in schema::SETTINGS
            .iter()
            .filter(|s| s.name.starts_with("settings."))
        {
            let (value, layer) = match (live(s.name), s.name) {
                (Some(v), _) => (v, "overlay"),
                (None, "settings.allowed") if !self.allowed.is_empty() => {
                    (serde_json::json!(self.allowed), "flake")
                }
                (None, "settings.allowed") => (serde_json::json!([]), "default"),
                (None, _) => (s.default_value().unwrap_or(Value::Null), "default"),
            };
            let last = self
                .last_change(s.name)
                .unwrap_or_else(|| "no change logged".into());
            out.push_str(&format!("{}  {value}  ({layer}; last: {last})\n", s.name));
        }
        out
    }

    /// The newest log entry that changed `name`: when, by whom, why.
    fn last_change(&self, name: &str) -> Option<String> {
        let log = fs::read_to_string(self.overlay.with_extension("log")).ok()?;
        log.lines().rev().find_map(|line| {
            let entry: Value = serde_json::from_str(line).ok()?;
            (entry["option"] == name).then(|| {
                format!(
                    "{} by {}{}",
                    entry["at"],
                    entry["actor"].as_str().unwrap_or("?"),
                    entry["reason"]
                        .as_str()
                        .map_or(String::new(), |r| format!(": {r}"))
                )
            })
        })
    }

    /// All mutations are absolute (safe to repeat), serialized and atomic.
    /// The audit is appended and synced before the new overlay is published.
    pub fn command(&self, args: &[String], actor: &str) -> Result<String, String> {
        self.change(args, actor, None)
    }

    pub fn request(
        &self,
        receipt: &std::path::Path,
        args: &[String],
        actor: &str,
    ) -> Result<String, String> {
        self.change(args, actor, Some(receipt.to_string_lossy().as_ref()))
    }

    fn change(
        &self,
        args: &[String],
        actor: &str,
        receipt: Option<&str>,
    ) -> Result<String, String> {
        // Refused before anything is touched: no lock, log or overlay.
        let request = parse(args)?;
        if request == Request::Show {
            return self.show();
        }
        let parent = self
            .overlay
            .parent()
            .ok_or("overlay needs a parent directory")?;
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let lock = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.overlay.with_extension("lock"))
            .map_err(|e| e.to_string())?;
        lock.lock().map_err(|e| e.to_string())?;
        let mut state = self.read()?;
        if let Some(body) = receipt.and_then(|r| state.receipts.get(r)) {
            return Ok(body.clone());
        }
        let (option, value, why, answer) = match request {
            Request::Show => unreachable!("answered above"),
            Request::Allow(repo) => {
                state.repos.insert(repo, true);
                (REPOS.to_owned(), None, None, CHANGED.to_owned())
            }
            Request::Pause(repo) => {
                state.repos.insert(repo, false);
                (REPOS.to_owned(), None, None, CHANGED.to_owned())
            }
            Request::Pipeline(t) => {
                state.pipeline_until = Some(t);
                (UNTIL.to_owned(), None, None, CHANGED.to_owned())
            }
            Request::Back => {
                state.pipeline_until = None;
                (UNTIL.to_owned(), None, None, CHANGED.to_owned())
            }
            Request::Set {
                name,
                value,
                reason,
            } => {
                let answer = format!("{name} set to {value}; it lasts {}.", lasts(&name));
                state.apply(&name, Some(&value))?;
                (name.clone(), Some(value), Some(reason), answer)
            }
            Request::Unset { name, reason } => {
                let answer = format!("{name} unset; it is back to its default.");
                state.apply(&name, None)?;
                (name.clone(), None, Some(reason), answer)
            }
        };
        if let Some(receipt) = receipt {
            state.receipts.insert(receipt.into(), answer.clone());
        }
        use std::io::Write;
        let mut log = File::options()
            .append(true)
            .create(true)
            .open(self.overlay.with_extension("log"))
            .map_err(|e| e.to_string())?;
        writeln!(
            log,
            "{}",
            serde_json::json!({
                "at": now(), "actor": actor, "change": args,
                "option": option, "value": value, "reason": why,
            })
        )
        .map_err(|e| e.to_string())?;
        log.sync_all().map_err(|e| e.to_string())?;
        let pending = self.overlay.with_extension("pending");
        let mut file = File::create(&pending).map_err(|e| e.to_string())?;
        file.write_all(&serde_json::to_vec(&state).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        // Readers include the isolated reviewer account. This overlay holds
        // public operational policy, regardless of the writer's umask.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o644))
                .map_err(|e| e.to_string())?;
        }
        file.sync_all().map_err(|e| e.to_string())?;
        fs::rename(pending, &self.overlay).map_err(|e| e.to_string())?;
        File::open(parent)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())?;
        if receipt.is_some() {
            Ok(answer)
        } else {
            Ok(format!("{answer}\n{}", self.show()?))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn one_policy_is_opt_in_live_and_scoped() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config {
            overlay: dir.path().join("overlay.json"),
            allowed: vec!["sam/box".into(), "sam/game".into()],
            pipeline_repos: vec!["sam/box".into()],
        };
        assert!(!cfg.allows("sam/new").unwrap());
        assert!(cfg.allows("SAM/GAME").unwrap());
        cfg.command(&["pipeline".into(), (now() + 60).to_string()], "test")
            .unwrap();
        assert!(!cfg.allows("sam/game").unwrap());
        assert!(cfg.allows("sam/box").unwrap());
        assert!(cfg.show().unwrap().contains("pipeline-only"));
        assert!(cfg.show().unwrap().contains("paused: sam/game"));
        cfg.command(&["back".into()], "test").unwrap();
        assert!(cfg.allows("sam/game").unwrap());
        cfg.command(&["pause".into(), "SAM/BOX".into()], "test")
            .unwrap();
        assert!(!cfg.allows("sam/box").unwrap());
        cfg.command(&["allow".into(), "sam/box".into()], "test")
            .unwrap();
        assert!(cfg.allows("sam/box").unwrap());
        fs::write(&cfg.overlay, r#"{"pipelineUntil":1}"#).unwrap();
        assert!(
            cfg.allows("sam/game").unwrap(),
            "scope expires without a daemon"
        );
        fs::write(&cfg.overlay, "bad JSON").unwrap();
        assert!(
            cfg.allows("sam/box").is_err(),
            "read errors cannot enable work"
        );
    }

    fn words(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_owned).collect()
    }

    fn cfg_in(dir: &tempfile::TempDir) -> Config {
        Config {
            overlay: dir.path().join("overlay.json"),
            allowed: vec!["sam/box".into()],
            pipeline_repos: vec![],
        }
    }

    #[test]
    fn a_live_option_round_trips_through_set_show_and_unset() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg_in(&dir);
        let shown = cfg.show().unwrap();
        assert!(
            shown.contains("settings.repositoryOverrides  {}  (default; last: no change logged)"),
            "{shown}"
        );
        assert!(
            shown.contains("settings.allowed  [\"sam/box\"]  (flake;"),
            "{shown}"
        );

        let said = cfg
            .command(
                &words(r#"set settings.repositoryOverrides {"SAM/Box":false} because testing the door"#),
                "alice",
            )
            .unwrap();
        assert!(said.contains("lasts until it is changed back"), "{said}");
        assert!(!cfg.allows("sam/box").unwrap());
        let shown = cfg.show().unwrap();
        assert!(
            shown.contains(r#"settings.repositoryOverrides  {"sam/box":false}  (overlay; last: "#),
            "{shown}"
        );
        assert!(shown.contains("by alice: testing the door"), "{shown}");

        cfg.command(
            &words("unset settings.repositoryOverrides because done testing"),
            "bob",
        )
        .unwrap();
        assert!(cfg.allows("sam/box").unwrap());
        let shown = cfg.show().unwrap();
        assert!(shown.contains("{}  (default; last: "), "{shown}");
        assert!(shown.contains("by bob: done testing"), "{shown}");
    }

    #[test]
    fn a_bad_value_is_refused_and_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg_in(&dir);
        for line in [
            "set settings.repositoryOverrides {\"sam/box\":\"no\"} because a wrong kind",
            "set settings.repositoryOverrides not-json because a wrong kind",
            "set settings.pipelineUntil -5 because a wrong kind",
            "set settings.pipelineUntil 99 because ok",
            "set settings.pipelineUntil 99",
            "set settings.nope 1 because no such option",
        ] {
            assert!(cfg.command(&words(line), "t").is_err(), "{line}");
        }
        let err = cfg
            .command(
                &words("set settings.pipelineUntil -5 because a wrong kind"),
                "t",
            )
            .unwrap_err();
        assert!(err.contains("settings.pipelineUntil: "), "{err}");
        assert!(!cfg.overlay.exists());
        assert!(!cfg.overlay.with_extension("log").exists());
    }

    #[test]
    fn an_option_that_is_not_live_is_refused_with_its_reason() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg_in(&dir);
        let err = cfg
            .command(&words("set autoland.rounds 3 because too slow"), "t")
            .unwrap_err();
        assert!(err.contains("autoland.rounds is not changed here"), "{err}");
        assert!(err.contains("built by Nix"), "{err}");
        let err = cfg
            .command(&words("unset proj.roles because clearing it"), "t")
            .unwrap_err();
        assert!(err.contains("proj's own file"), "{err}");
        assert!(!cfg.overlay.exists());
    }

    #[test]
    fn show_reports_the_layer_each_value_comes_from() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg_in(&dir);
        let line = |name: &str| {
            cfg.show()
                .unwrap()
                .lines()
                .find(|l| l.starts_with(name))
                .unwrap()
                .to_owned()
        };
        assert!(line("settings.allowed").contains("(flake;"));
        assert!(line("settings.pipelineUntil").contains("(default;"));
        let until = now() + 600;
        cfg.command(
            &words(&format!(
                "set settings.pipelineUntil {until} because a short scope"
            )),
            "t",
        )
        .unwrap();
        let l = line("settings.pipelineUntil");
        assert!(l.contains(&format!("{until}  (overlay;")), "{l}");
        let bare = Config {
            overlay: cfg.overlay.clone(),
            ..Default::default()
        };
        let l = bare.show().unwrap();
        assert!(l.contains("settings.allowed  []  (default;"), "{l}");
        cfg.command(&words("back"), "t").unwrap();
        assert!(line("settings.pipelineUntil").contains("(default;"));
    }

    #[test]
    fn a_retried_issue_cannot_undo_a_newer_pause() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config {
            overlay: dir.path().join("overlay.json"),
            ..Default::default()
        };
        let receipt = dir.path().join("issue-open-cycle");
        cfg.request(&receipt, &["allow".into(), "sam/box".into()], "github")
            .unwrap();
        cfg.command(&["pause".into(), "sam/box".into()], "host")
            .unwrap();
        cfg.request(&receipt, &["allow".into(), "sam/box".into()], "github")
            .unwrap();
        assert!(!cfg.allows("sam/box").unwrap());
    }
    #[test]
    #[cfg(unix)]
    fn the_overlay_is_readable_by_isolated_consumers() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config {
            overlay: dir.path().join("overlay.json"),
            ..Default::default()
        };
        let pending = cfg.overlay.with_extension("pending");
        fs::write(&pending, "interrupted").unwrap();
        burrow_test::permissions(&pending, 0o600);
        cfg.command(&["pause".into(), "sam/box".into()], "test")
            .unwrap();
        assert_eq!(
            fs::metadata(&cfg.overlay).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }
}
