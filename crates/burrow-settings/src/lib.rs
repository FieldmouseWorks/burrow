//! The tracked admission policy plus one live overlay. Every decision reloads
//! the overlay; an unreadable policy fails closed. No pipeline daemon is needed
//! to change it. Other settings can join this document without another store.
pub mod door;
pub mod roles;
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
    /// Which engine, model and effort runs each role, over the schema's
    /// defaults ([`roles`]).
    #[serde(default)]
    routing: roles::Roles,
    /// What each engine offers, where it isn't the schema's default.
    #[serde(default)]
    offers: BTreeMap<String, roles::Offer>,
    #[serde(default)]
    setups: roles::Setups,
    /// When proj's old roles and models files were moved in, once.
    #[serde(default)]
    migrated: Option<i64>,
}

/// proj's old roles, models and setups files, read once into the overlay
/// ([`Config::migrate`]).
#[derive(Debug, Default)]
pub struct Migration {
    pub routing: roles::Roles,
    /// Models and efforts the old files set, which the engines offered.
    pub offers: BTreeMap<String, roles::Offer>,
    pub setups: roles::Setups,
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

    /// The role settings in force, checked again on every read: an overlay
    /// a hand edit broke fails the turn rather than running a default.
    pub fn roles(&self) -> Result<roles::InForce, String> {
        let state = self.read()?;
        state.routing.in_force(&state.offers)
    }

    /// Whether the role settings are set in the overlay rather than
    /// defaulted: (routes, tunings, offers).
    pub fn roles_set(&self) -> Result<(bool, bool, bool), String> {
        let state = self.read()?;
        Ok((
            !state.routing.roles.is_empty(),
            !state.routing.models.is_empty(),
            !state.offers.is_empty(),
        ))
    }

    /// The saved setups and the one in force, as `settings show` gives
    /// them; None when none is saved.
    pub fn setups(&self) -> Result<Option<serde_json::Value>, String> {
        let state = self.read()?;
        if state.setups.saved.is_empty() {
            return Ok(None);
        }
        Ok(Some(serde_json::json!({
            "active": roles::active(&state.routing, &state.setups),
            "saved": state.setups.saved,
        })))
    }

    /// `engines`: the setups and the routes in force, changing nothing.
    pub fn engines(&self) -> Result<String, String> {
        let state = self.read()?;
        state.routing.in_force(&state.offers)?;
        let active = roles::active(&state.routing, &state.setups);
        Ok(roles::status(
            active.as_deref(),
            &state.routing,
            &state.setups,
        ))
    }

    /// Moves proj's old files in, once: false, changing nothing, when the
    /// overlay already took them. Checked as the door checks a change.
    pub fn migrate(&self, from: Migration, actor: &str) -> Result<bool, String> {
        let mut moved = false;
        self.write(actor, None, &["migrate".into()], |state| {
            if state.migrated.is_some() {
                return Ok(None);
            }
            let mut offers = roles::default_offers();
            offers.extend(state.offers.clone());
            for (engine, offer) in from.offers {
                let have = offers.entry(engine).or_default();
                for m in offer.models {
                    if !have.models.contains(&m) {
                        have.models.push(m);
                    }
                }
                for e in offer.efforts {
                    if !have.efforts.contains(&e) {
                        have.efforts.push(e);
                    }
                }
            }
            from.routing.in_force(&offers)?;
            for (name, setup) in &from.setups.saved {
                if !roles::valid_setup(name) {
                    return Err(format!("setup {name}: not a setup name"));
                }
                setup
                    .check(&offers)
                    .map_err(|e| format!("setup {name}: {e}"))?;
            }
            // Only what differs from the declared default is set.
            let declared = roles::default_offers();
            offers.retain(|engine, offer| declared.get(engine) != Some(offer));
            state.offers = offers;
            state.routing = from.routing;
            state.setups = from.setups;
            state.migrated = Some(now());
            moved = true;
            Ok(Some(
                "Moved proj's roles and models into the settings.".into(),
            ))
        })?;
        Ok(moved)
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

    /// What the request with RECEIPT was answered, once its change is in
    /// the overlay.
    pub fn answered(&self, receipt: &std::path::Path) -> Result<Option<String>, String> {
        Ok(self
            .read()?
            .receipts
            .get(receipt.to_string_lossy().as_ref())
            .cloned())
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
        let words: Vec<&str> = args.iter().map(String::as_str).collect();
        match words.as_slice() {
            ["show"] => return self.show(),
            ["engines"] | ["engines", "list"] => return self.engines(),
            _ => {}
        }
        let answer = self.write(actor, receipt, args, |state| {
            match words.as_slice() {
                [verb @ ("allow" | "pause"), repo] if burrow_github::repo_name(repo) => {
                    state.repos.insert(repo.to_ascii_lowercase(), *verb == "allow");
                }
                ["pipeline", until] => {
                    let t: i64 = until.parse().map_err(|_| "pipeline needs a Unix timestamp")?;
                    if t <= now() {
                        return Err("pipeline deadline must be in the future".into());
                    }
                    state.pipeline_until = Some(t);
                }
                ["back"] => state.pipeline_until = None,
                ["role" | "model" | "offer" | "engines", ..] => {
                    return roles::apply(
                        &mut state.routing,
                        &mut state.offers,
                        &mut state.setups,
                        &words,
                    )
                    .map(Some);
                }
                _ => {
                    return Err(format!(
                        "usage: settings show | allow OWNER/REPO | pause OWNER/REPO | pipeline UNTIL | back\n{}",
                        roles::USAGE
                    ));
                }
            }
            Ok(Some(String::new()))
        })?;
        match answer {
            Some(said) if !said.is_empty() => Ok(said),
            _ if receipt.is_some() => {
                Ok("Settings changed; use settings show for the current policy.".into())
            }
            _ => self.show(),
        }
    }

    /// One change, serialized and atomic. CHANGE returns None to leave the
    /// overlay as it is, or the change's answer. A receipt already in the
    /// overlay answers again with what it said, changing nothing. The log
    /// line is appended and synced before the new overlay is published.
    fn write(
        &self,
        actor: &str,
        receipt: Option<&str>,
        args: &[String],
        change: impl FnOnce(&mut Overlay) -> Result<Option<String>, String>,
    ) -> Result<Option<String>, String> {
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
            return Ok(Some(body.clone()));
        }
        let Some(answer) = change(&mut state)? else {
            return Ok(None);
        };
        if let Some(receipt) = receipt {
            let said = if answer.is_empty() {
                "Settings changed; use settings show for the current policy."
            } else {
                &answer
            };
            state.receipts.insert(receipt.into(), said.into());
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
            serde_json::json!({"at": now(), "actor": actor, "change": args})
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
        Ok(Some(answer))
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
