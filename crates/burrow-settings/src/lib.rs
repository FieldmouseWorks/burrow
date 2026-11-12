//! The tracked admission policy plus one live overlay. Every decision reloads
//! the overlay; an unreadable policy fails closed. No pipeline daemon is needed
//! to change it. Other settings can join this document without another store.
pub mod door;
pub mod engines;
mod legacy;
#[cfg(test)]
mod review_tests;
use serde::{Deserialize, Serialize};
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
    /// The directory that held proj's roles, models and roles.d files, read
    /// once into the overlay ([`Config::migrate`]).
    #[serde(default)]
    pub legacy: Option<PathBuf>,
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
    /// Kept in `engines.json` beside the overlay, not in it: a previous
    /// generation that rolls back still reads the overlay it knows.
    #[serde(skip)]
    engines: engines::Engines,
}

/// Only a semantic refusal is safe to acknowledge without a completion receipt.
/// Storage and migration failures leave the operation's outcome uncertain.
#[derive(Debug)]
pub enum RequestError {
    Refused(String),
    Retry(String),
}

impl From<String> for RequestError {
    fn from(error: String) -> Self {
        Self::Retry(error)
    }
}
impl From<&str> for RequestError {
    fn from(error: &str) -> Self {
        Self::Retry(error.into())
    }
}
impl std::fmt::Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(e) | Self::Retry(e) => f.write_str(e),
        }
    }
}

/// A visible result must also be durable before a caller acknowledges it.
fn sync_file(path: &std::path::Path) -> Result<(), String> {
    #[cfg(test)]
    review_tests::flush_failure()?;
    File::open(path)
        .and_then(|f| f.sync_all())
        .and_then(|()| File::open(path.parent().unwrap_or(std::path::Path::new("."))))
        .and_then(|d| d.sync_all())
        .map_err(|e| format!("{}: {e}", path.display()))
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// The door validates requests before attempting mutations; malformed issues
/// are answered rather than blocking later off requests.
pub fn valid_command(args: &[String]) -> bool {
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["show" | "back"] => true,
        ["allow" | "pause", repo] => burrow_github::repo_name(repo),
        ["pipeline", until] => until.parse::<i64>().is_ok_and(|t| t > now()),
        _ => false,
    }
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

    fn read_overlay(&self) -> Result<Overlay, String> {
        match fs::read(&self.overlay) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| e.to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Overlay::default()),
            Err(e) => Err(e.to_string()),
        }
    }

    fn read_engines(&self) -> Result<Option<engines::Engines>, String> {
        let path = self.engines_path();
        match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !Self::present(&path)? => {
                Ok(None)
            }
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    fn read(&self) -> Result<Overlay, String> {
        let mut overlay = self.read_overlay()?;
        overlay.engines = self.read_engines()?.unwrap_or_default();
        Ok(overlay)
    }

    /// The file the engine settings live in.
    pub fn engines_path(&self) -> PathBuf {
        self.overlay.with_file_name("engines.json")
    }

    /// Flush the engine store and its directory, for a result that must
    /// outlast a crash before anyone is told of it.
    pub fn sync_engines(&self) -> Result<(), String> {
        sync_file(&self.engines_path())
    }

    /// The stored answer to an engines request's receipt, if it completed.
    pub fn receipt(&self, receipt: &std::path::Path) -> Result<Option<String>, String> {
        let engines = self.read()?.engines;
        engines.validate()?;
        let body = engines
            .receipts
            .get(receipt.to_string_lossy().as_ref())
            .cloned();
        if body.is_some() {
            self.sync_engines()?;
        }
        Ok(body)
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
            "Automated work: {}; allowed: {}; paused: {}; all unlisted repos paused (including new repos)\n",
            scoped.map_or("allow list".into(), |t| format!("pipeline-only until {t}")),
            allowed.join(", "),
            paused.join(", ")
        ))
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

    /// Engine requests distinguish a refused command from an uncertain write.
    /// Legacy receipts are adopted under the same lock as legacy migration.
    pub fn engine_request(
        &self,
        receipt: &std::path::Path,
        args: &[String],
        actor: &str,
    ) -> Result<String, RequestError> {
        self.change_checked(args, actor, Some(receipt.to_string_lossy().as_ref()))
    }

    fn change(
        &self,
        args: &[String],
        actor: &str,
        receipt: Option<&str>,
    ) -> Result<String, String> {
        self.change_checked(args, actor, receipt)
            .map_err(|e| e.to_string())
    }

    fn change_checked(
        &self,
        args: &[String],
        actor: &str,
        receipt: Option<&str>,
    ) -> Result<String, RequestError> {
        if args == ["show"] {
            return self.show().map_err(Into::into);
        }
        if matches!(args, [e] | [e, _] if e == "engines") && args.get(1).is_none_or(|a| a == "list")
        {
            return Ok(self.engines()?.status(None));
        }
        let _lock = self.lock()?;
        let _legacy_lock = self.legacy_lock()?;
        let engine_verb = matches!(
            args.first().map(String::as_str),
            Some("role" | "unrole" | "offer" | "model" | "unmodel" | "engines")
        );
        if engine_verb && let Some(receipt) = receipt {
            if let Some(body) = self.receipt(std::path::Path::new(receipt))? {
                return Ok(body);
            }
            if let Some(body) = legacy::result(std::path::Path::new(receipt))? {
                return Ok(body);
            }
        }
        self.migrate_locked()?;
        let mut state = self.read()?;
        if engine_verb {
            state.engines.validate()?;
        }
        let mut engines_answer = None;
        if let Some(body) = receipt.and_then(|r| {
            state
                .receipts
                .get(r)
                .or_else(|| state.engines.receipts.get(r))
        }) {
            return Ok(body.clone());
        }
        match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
            [verb @ ("allow" | "pause"), repo] if burrow_github::repo_name(repo) => {
                state.repos.insert(repo.to_ascii_lowercase(), *verb == "allow");
            }
            ["pipeline", until] => {
                let t: i64 = until.parse().map_err(|_| "pipeline needs a Unix timestamp")?;
                if t <= now() { return Err("pipeline deadline must be in the future".into()); }
                state.pipeline_until = Some(t);
            }
            ["back"] => state.pipeline_until = None,
            ["role", role, spec @ ..] => state.engines.set_role(role, spec)?,
            ["unrole", role] => state.engines.clear_role(role)?,
            ["offer", on, models @ ..] if !models.is_empty() => state.engines.offer(on, models)?,
            ["model", rest @ ..] => state.engines.set_model(rest)?,
            ["unmodel", role, on] => state.engines.clear_model(role, on)?,
            ["engines", rest @ ..] => engines_answer = Some(state.engines.apply(rest, now()).map_err(RequestError::Refused)?),
            _ => return Err("usage: settings show | allow OWNER/REPO | pause OWNER/REPO | pipeline UNTIL | back | role ROLE ENGINE [over PCT ENGINE] | unrole ROLE | offer ENGINE MODEL... | model ROLE ENGINE MODEL|- [EFFORT] | unmodel ROLE ENGINE | engines [list | save NAME [--replace] | use NAME|back]".into()),
        }
        let answer = engines_answer.clone().unwrap_or_else(|| {
            "Settings changed; use settings show for the current policy.".to_owned()
        });
        if let Some(receipt) = receipt {
            let book = if engine_verb {
                &mut state.engines.receipts
            } else {
                &mut state.receipts
            };
            book.insert(receipt.into(), answer.clone());
        }
        self.persist(&state, actor, serde_json::json!(args), engine_verb)?;
        if engine_verb && receipt.is_some() {
            self.sync_engines()?;
        }
        if receipt.is_some() || engines_answer.is_some() {
            Ok(answer)
        } else {
            self.show().map_err(Into::into)
        }
    }

    /// Append the audit line, then publish the overlay atomically.
    fn persist(
        &self,
        state: &Overlay,
        actor: &str,
        change: serde_json::Value,
        engines: bool,
    ) -> Result<(), String> {
        let parent = self
            .overlay
            .parent()
            .ok_or("overlay needs a parent directory")?;
        use std::io::Write;
        let mut log = File::options()
            .append(true)
            .create(true)
            .open(self.overlay.with_extension("log"))
            .map_err(|e| e.to_string())?;
        writeln!(
            log,
            "{}",
            serde_json::json!({"at": now(), "actor": actor, "change": change})
        )
        .map_err(|e| e.to_string())?;
        log.sync_all().map_err(|e| e.to_string())?;
        let (target, bytes) = if engines {
            (self.engines_path(), serde_json::to_vec(&state.engines))
        } else {
            (self.overlay.clone(), serde_json::to_vec(state))
        };
        let pending = target.with_extension("pending");
        let mut file = File::create(&pending).map_err(|e| e.to_string())?;
        file.write_all(&bytes.map_err(|e| e.to_string())?)
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
        fs::rename(pending, &target).map_err(|e| e.to_string())?;
        #[cfg(test)]
        review_tests::publication_failure()?;
        File::open(parent)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())
    }

    /// The engines, models and saved sets in force, read fresh.
    pub fn engines(&self) -> Result<engines::Engines, String> {
        self.engine_snapshot().map(|(engines, _)| engines)
    }

    /// Values and their source from one read, including a read-only legacy import.
    pub fn engine_snapshot(&self) -> Result<(engines::Engines, PathBuf), String> {
        self.read_overlay()?;
        let (engines, source) = match self.read_engines()? {
            Some(engines) => (engines, self.engines_path()),
            None => match self.legacy_engines()? {
                Some(engines) => (
                    engines,
                    self.legacy.clone().expect("legacy values have a directory"),
                ),
                None => (Default::default(), self.engines_path()),
            },
        };
        engines
            .validate()
            .map_err(|e| format!("{}: {e}", source.display()))?;
        Ok((engines, source))
    }

    /// Replace the roles and models with those the old file formats say
    /// (`ROLE ENGINE [over PCT ENGINE]` and `ROLE ENGINE MODEL|- [EFFORT]`
    /// lines), logged. Models they name become offered. Saved sets stay.
    pub fn import(&self, roles: &str, models: &str, actor: &str) -> Result<(), String> {
        let _lock = self.lock()?;
        let _legacy_lock = self.legacy_lock()?;
        self.migrate_locked()?;
        let new = engines::import(roles, models, &[], None)?;
        let mut state = self.read()?;
        state.engines.roles = new.roles;
        state.engines.models = new.models;
        for (on, models) in new.offered {
            let refs: Vec<&str> = models.iter().map(String::as_str).collect();
            state.engines.offer(&on, &refs)?;
        }
        self.persist(&state, actor, serde_json::json!(["import"]), true)
    }

    /// Read the old roles, models and roles.d files into `engines.json`,
    /// once: a second call finds it there and does nothing. The old files stay.
    /// A file that doesn't parse, or a link to nowhere, stops it with
    /// nothing written.
    pub fn migrate(&self) -> Result<bool, String> {
        if self.legacy.is_none() {
            return Ok(false);
        }
        let _lock = self.lock()?;
        let _legacy_lock = self.legacy_lock()?;
        self.migrate_locked()
    }

    fn lock(&self) -> Result<File, String> {
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
        Ok(lock)
    }

    /// The writer lock and legacy lock are held until publication completes.
    fn migrate_locked(&self) -> Result<bool, String> {
        if Self::present(&self.engines_path())? {
            return Ok(false);
        }
        let Some(old) = self.legacy_engines()? else {
            return Ok(false);
        };
        let state = Overlay {
            engines: old,
            ..self.read()?
        };
        self.persist(
            &state,
            "migration",
            serde_json::json!(["migrate", self.legacy]),
            true,
        )?;
        Ok(true)
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
            ..Default::default()
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

pub mod schema;
pub mod show;
