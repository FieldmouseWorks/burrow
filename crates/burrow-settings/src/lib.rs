//! The tracked admission policy plus one live overlay. Every decision reloads
//! the overlay; an unreadable policy fails closed. No pipeline daemon is needed
//! to change it. Other settings can join this document without another store.
pub mod door;
pub mod engines;
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
    #[serde(default, skip_serializing_if = "engines::Engines::is_empty")]
    engines: engines::Engines,
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
        ["engines", "list"] => true,
        ["engines", "use", name] => engines::valid_name(name),
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

    fn change(
        &self,
        args: &[String],
        actor: &str,
        receipt: Option<&str>,
    ) -> Result<String, String> {
        if args == ["show"] {
            return self.show();
        }
        if matches!(args, [e] | [e, _] if e == "engines") && args.get(1).is_none_or(|a| a == "list")
        {
            return Ok(self.read()?.engines.status(None));
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
        let mut engines_answer = None;
        if let Some(body) = receipt.and_then(|r| state.receipts.get(r)) {
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
            ["engines", rest @ ..] => engines_answer = Some(state.engines.apply(rest, now())?),
            _ => return Err("usage: settings show | allow OWNER/REPO | pause OWNER/REPO | pipeline UNTIL | back | role ROLE ENGINE [over PCT ENGINE] | unrole ROLE | offer ENGINE MODEL... | model ROLE ENGINE MODEL|- [EFFORT] | unmodel ROLE ENGINE | engines [list | save NAME [--replace] | use NAME|back]".into()),
        }
        let answer = engines_answer.clone().unwrap_or_else(|| {
            "Settings changed; use settings show for the current policy.".to_owned()
        });
        if let Some(receipt) = receipt {
            state.receipts.insert(receipt.into(), answer.clone());
        }
        self.persist(&state, actor, serde_json::json!(args))?;
        if receipt.is_some() || engines_answer.is_some() {
            Ok(answer)
        } else {
            self.show()
        }
    }

    /// Append the audit line, then publish the overlay atomically.
    fn persist(
        &self,
        state: &Overlay,
        actor: &str,
        change: serde_json::Value,
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
        let pending = self.overlay.with_extension("pending");
        let mut file = File::create(&pending).map_err(|e| e.to_string())?;
        file.write_all(&serde_json::to_vec(state).map_err(|e| e.to_string())?)
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
            .map_err(|e| e.to_string())
    }

    /// The engines, models and saved sets in force, read fresh.
    pub fn engines(&self) -> Result<engines::Engines, String> {
        Ok(self.read()?.engines)
    }

    /// Replace the roles and models with those the old file formats say
    /// (`ROLE ENGINE [over PCT ENGINE]` and `ROLE ENGINE MODEL|- [EFFORT]`
    /// lines), logged. Models they name become offered. Saved sets stay.
    pub fn import(&self, roles: &str, models: &str, actor: &str) -> Result<(), String> {
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
        let new = engines::import(roles, models, &[], None)?;
        let mut state = self.read()?;
        state.engines.roles = new.roles;
        state.engines.models = new.models;
        for (on, models) in new.offered {
            let refs: Vec<&str> = models.iter().map(String::as_str).collect();
            state.engines.offer(&on, &refs)?;
        }
        self.persist(&state, actor, serde_json::json!(["import"]))
    }

    /// Move the old roles, models and roles.d files into the overlay, once:
    /// each is renamed `NAME.migrated` after the overlay holds its values, so
    /// a second call finds nothing. Anything already set in the overlay wins.
    /// A file that doesn't parse stops it with the old files untouched.
    pub fn migrate(&self) -> Result<bool, String> {
        let Some(dir) = &self.legacy else {
            return Ok(false);
        };
        let names = ["roles", "models", "roles.d", "roles.previous"];
        if !names.iter().any(|n| dir.join(n).symlink_metadata().is_ok()) {
            return Ok(false);
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
        let text = |name: &str| match fs::read_to_string(dir.join(name)) {
            Ok(t) => Ok(t),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(e) => Err(format!("{}: {e}", dir.join(name).display())),
        };
        let mut setups = Vec::new();
        match fs::read_dir(dir.join("roles.d")) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry.map_err(|e| e.to_string())?;
                    if entry.file_type().map_err(|e| e.to_string())?.is_file() {
                        let name = entry.file_name().to_string_lossy().into_owned();
                        let body = fs::read_to_string(entry.path())
                            .map_err(|e| format!("{}: {e}", entry.path().display()))?;
                        setups.push((name, body));
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{}: {e}", dir.join("roles.d").display())),
        }
        let previous = text("roles.previous")?.trim().to_owned();
        let old = engines::import(
            &text("roles")?,
            &text("models")?,
            &setups,
            (!previous.is_empty()).then_some(previous),
        )
        .map_err(|e| format!("{}: {e}", dir.display()))?;
        let mut state = self.read()?;
        let mine = &mut state.engines;
        for (k, v) in old.roles {
            mine.roles.entry(k).or_insert(v);
        }
        for (k, v) in old.models {
            mine.models.entry(k).or_insert(v);
        }
        for (on, models) in old.offered {
            let refs: Vec<&str> = models.iter().map(String::as_str).collect();
            mine.offer(&on, &refs)?;
        }
        for (k, v) in old.saved {
            mine.saved.entry(k).or_insert(v);
        }
        if mine.previous.is_none() {
            mine.previous = old.previous;
        }
        self.persist(&state, "migration", serde_json::json!(["migrate", dir]))?;
        for name in names {
            let from = dir.join(name);
            if from.symlink_metadata().is_ok() {
                fs::rename(&from, dir.join(format!("{name}.migrated")))
                    .map_err(|e| format!("{}: {e}", from.display()))?;
            }
        }
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
