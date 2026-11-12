//! The tracked admission policy plus one live overlay. Every decision reloads
//! the overlay; an unreadable policy fails closed. No pipeline daemon is needed
//! to change it. Other settings can join this document without another store.
pub mod door;
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

/// The owner's pause of Claude work: no Claude turn takes a slot until it
/// ends. Codex work is not affected.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UsageHold {
    pub until: i64,
    pub reason: String,
    /// When it was set.
    pub since: i64,
}

/// How the owner's last pauses ended, kept by the door beside the overlay so
/// every way of clearing a hold leaves it: when, the admission cutoff its
/// queue is cut at (nanoseconds), and for a deadline that passed, the
/// deadline before which a refusal is a stale run's. Each only grows.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UsageEnd {
    pub release: i64,
    pub cutoff: i64,
    pub stale: i64,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Overlay {
    #[serde(default)]
    usage_hold: Option<UsageHold>,
    #[serde(default)]
    repos: BTreeMap<String, bool>,
    #[serde(default)]
    pipeline_until: Option<i64>,
    #[serde(default)]
    receipts: BTreeMap<String, String>,
}

fn nanos() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as i64
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
        ["show" | "back" | "usage-back"] => true,
        ["usage-hold", until, reason @ ..] => {
            !reason.is_empty() && until.parse::<i64>().is_ok_and(|t| t > now())
        }
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

    /// The usage hold in force now: an expired one is none, with no daemon.
    pub fn usage_hold(&self) -> Result<Option<UsageHold>, String> {
        Ok(self.usage_hold_record()?.filter(|h| h.until > now()))
    }

    /// The hold as recorded, expired or not: whoever ends one closes its notice.
    pub fn usage_hold_record(&self) -> Result<Option<UsageHold>, String> {
        Ok(self.read()?.usage_hold)
    }

    fn end_path(&self) -> PathBuf {
        self.overlay.with_extension("usage-end")
    }

    /// The journal of ended pauses: absent is none, anything unreadable is an
    /// error, so no caller takes damage for no history.
    pub fn usage_end(&self) -> Result<UsageEnd, String> {
        match fs::read(self.end_path()) {
            Ok(b) => serde_json::from_slice(&b)
                .map_err(|e| format!("{}: {e}", self.end_path().display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(UsageEnd::default()),
            Err(e) => Err(format!("{}: {e}", self.end_path().display())),
        }
    }

    /// Records an end (each field only grows), durably, before the overlay
    /// changes. Callers other than the door hold no lock: it is idempotent.
    pub fn record_usage_end(&self, end: UsageEnd) -> Result<(), String> {
        use std::io::Write;
        let old = self.usage_end()?;
        let new = UsageEnd {
            release: old.release.max(end.release),
            cutoff: old.cutoff.max(end.cutoff),
            stale: old.stale.max(end.stale),
        };
        let pending = self.end_path().with_extension("usage-end-pending");
        let mut f = File::create(&pending).map_err(|e| e.to_string())?;
        f.write_all(&serde_json::to_vec(&new).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            f.set_permissions(fs::Permissions::from_mode(0o644))
                .map_err(|e| e.to_string())?;
        }
        f.sync_all().map_err(|e| e.to_string())?;
        fs::rename(pending, self.end_path()).map_err(|e| e.to_string())
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
            // Always allowed, at once: no other daemon is asked.
            // A reason is the rest of the words, as the issue door splits them.
            ["usage-hold", until, reason @ ..] if !reason.join(" ").trim().is_empty() => {
                let until: i64 = until.parse().map_err(|_| "usage-hold needs a Unix timestamp")?;
                let at = now();
                if until <= at { return Err("usage hold deadline must be in the future".into()); }
                state.usage_hold = Some(UsageHold { until, reason: reason.join(" "), since: at });
            }
            ["usage-back"] => {
                if let Some(h) = state.usage_hold.as_ref().filter(|h| h.until > now()) {
                    self.record_usage_end(UsageEnd {
                        release: now(),
                        cutoff: nanos().min(h.until * 1_000_000_000),
                        stale: 0,
                    })?;
                }
                state.usage_hold = None;
            }
            // Cleanups that retire only the hold they examined, decided here
            // under the write lock: a newer hold or release stands.
            ["usage-expire", until] if state.usage_hold.as_ref().is_some_and(|h| {
                until.parse() == Ok(h.until) && h.until <= now()
            }) => {
                let until = state.usage_hold.as_ref().map_or(0, |h| h.until);
                self.record_usage_end(UsageEnd {
                    release: until,
                    cutoff: until * 1_000_000_000,
                    stale: until,
                })?;
                state.usage_hold = None;
            }
            ["usage-expire", _] => {}
            ["usage-migrate", until, reason @ ..]
                if state.usage_hold.is_none() && !reason.is_empty() =>
            {
                let until: i64 = until.parse().map_err(|_| "usage-migrate needs a Unix timestamp")?;
                if until > now() {
                    state.usage_hold = Some(UsageHold { until, reason: reason.join(" "), since: now() });
                }
            }
            ["usage-migrate", ..] => {}
            _ => return Err("usage: settings show | allow OWNER/REPO | pause OWNER/REPO | pipeline UNTIL | back | usage-hold UNTIL REASON | usage-back".into()),
        }
        let answer = "Settings changed; use settings show for the current policy.".to_owned();
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
        if receipt.is_some() {
            Ok(answer)
        } else {
            self.show()
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

    #[test]
    fn the_usage_hold_is_set_and_released_through_the_door_and_logged() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config {
            overlay: dir.path().join("overlay.json"),
            ..Default::default()
        };
        let until = now() + 600;
        cfg.command(
            &["usage-hold".into(), until.to_string(), "save usage".into()],
            "proj usage hold",
        )
        .unwrap();
        let hold = cfg.usage_hold().unwrap().unwrap();
        assert_eq!((hold.until, hold.reason.as_str()), (until, "save usage"));
        cfg.command(&["usage-back".into()], "proj usage back")
            .unwrap();
        assert_eq!(cfg.usage_hold().unwrap(), None);
        let log = fs::read_to_string(cfg.overlay.with_extension("log")).unwrap();
        assert!(log.contains("proj usage hold") && log.contains("save usage"));
        assert!(log.contains("proj usage back") && log.contains("usage-back"));
        assert!(
            cfg.command(&["usage-hold".into(), "1".into(), "late".into()], "t")
                .is_err()
        );
        fs::write(
            &cfg.overlay,
            r#"{"usageHold":{"until":1,"reason":"x","since":0}}"#,
        )
        .unwrap();
        assert_eq!(
            cfg.usage_hold().unwrap(),
            None,
            "a hold expires without a daemon"
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

#[cfg(test)]
mod end_tests {
    use super::*;
    #[test]
    fn every_way_of_clearing_a_hold_leaves_its_release_and_a_bad_journal_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config {
            overlay: dir.path().join("overlay.json"),
            ..Default::default()
        };
        let until = now() + 600;
        cfg.command(&["usage-hold".into(), until.to_string(), "x".into()], "t")
            .unwrap();
        cfg.command(&["usage-back".into()], "issue").unwrap();
        let end = cfg.usage_end().unwrap();
        assert!(end.release > 0 && end.cutoff > 0 && end.cutoff <= until * 1_000_000_000);
        fs::write(cfg.overlay.with_extension("usage-end"), "{trunc").unwrap();
        assert!(cfg.usage_end().is_err(), "unreadable is not no history");
    }
}
