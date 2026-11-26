//! The tracked admission policy plus one live overlay. Every decision reloads
//! the overlay; an unreadable policy fails closed. No pipeline daemon is needed
//! to change it. Other settings can join this document without another store.
pub mod credit;
pub mod door;
pub mod landing;
mod live;
pub use live::{path_exists, publish, receipt_key};
pub mod roles;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    pub overlay: PathBuf,
    /// Legacy role inputs. Role mutations wait for their migration; admission
    /// remains independent and available throughout the cutover.
    #[serde(default)]
    pub legacy_roles: Option<PathBuf>,
    /// The retained engines door's whole-pass lock, fenced at deployment.
    #[serde(default)]
    pub legacy_engines_lock: Option<PathBuf>,
    #[serde(default)]
    pub allowed: Vec<String>,
    #[serde(default)]
    pub pipeline_repos: Vec<String>,
    /// Who holds each repo's landing now (landing::Record), written by
    /// burrow-landing; null on a machine with no conaryd to hand one to,
    /// where every repo is the steward's.
    #[serde(default)]
    pub landing_record: Option<PathBuf>,
    /// The repos `landing OWNER/REPO conaryd` may give to conaryd: those it
    /// has a flow configuration for (burrow.landing.conaryd).
    #[serde(default)]
    pub conaryd: Vec<String>,
    /// Configured engine plugin names on this machine (from Nix engine plugin
    /// definitions). Empty falls back to the schema's declared defaults plus
    /// any engine registered via `offer`.
    #[serde(default)]
    pub engines: Vec<String>,
}

/// A scheduling decision and its next change without another external event.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Decision {
    pub allowed: bool,
    pub retry_at: Option<i64>,
    pub vacate: bool,
    /// The repo's PRs handed to the steward while it's paused: each lets
    /// that one PR, at that one head, settle and land, and nothing else.
    pub handed: Vec<Handed>,
    /// Who holds the repo's landing now: the steward lands nothing unless
    /// it's `owner` with no handover under way.
    pub landing: landing::Holding,
}

/// One PR a trusted request handed to the steward despite its repo's pause
/// (`land OWNER/REPO#N HEAD`). One-shot: the steward spends it once the PR
/// has landed or closed, and a pause of its repo revokes it.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Handed {
    pub pr: u64,
    pub head: String,
    pub at: i64,
    pub actor: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Overlay {
    #[serde(default)]
    repos: BTreeMap<String, bool>,
    #[serde(default)]
    pipeline_until: Option<i64>,
    #[serde(default)]
    receipts: BTreeMap<String, String>,
    /// Keyed by `owner/repo#n`, lowercase. Left out when there are none, so
    /// a generation that predates handoffs still reads the overlay.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    handoffs: BTreeMap<String, Handed>,
    /// Each repo's configured landing owner where it isn't the steward,
    /// keyed lowercase (`landing OWNER/REPO conaryd`). Left out when empty,
    /// as handoffs are.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    landing: BTreeMap<String, landing::Owner>,
}

/// `OWNER/REPO#N` as its handoff key, lowercase, and its parts.
pub fn handoff(target: &str) -> Option<(String, String, u64)> {
    let (repo, n) = target.split_once('#')?;
    let n: u64 = n.parse().ok().filter(|n| *n > 0)?;
    if !burrow_github::repo_name(repo) || n.to_string() != target.split_once('#')?.1 {
        return None;
    }
    let repo = repo.to_ascii_lowercase();
    Some((format!("{repo}#{n}"), repo, n))
}

/// A full commit id, as GitHub names a PR's head.
fn commit(head: &str) -> bool {
    head.len() == 40 && head.bytes().all(|b| b.is_ascii_hexdigit())
}

/// One immutable policy generation, including the time used for scope expiry.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Snapshot {
    config: Config,
    state: Overlay,
    record: landing::Record,
    at: i64,
}

impl Snapshot {
    pub fn decision(&self, repo: &str) -> Decision {
        let allowed = self.config.permits_at(&self.state, repo, self.at);
        Decision {
            allowed,
            vacate: self.state.repos.get(&repo.to_ascii_lowercase()) == Some(&false)
                || (self.state.pipeline_until.is_some_and(|t| t > self.at)
                    && !self
                        .config
                        .pipeline_repos
                        .iter()
                        .any(|r| r.eq_ignore_ascii_case(repo))),
            retry_at: self
                .state
                .pipeline_until
                .filter(|t| !allowed && *t > self.at),
            handed: handed(&self.state, repo),
            landing: self.record.holding(repo),
        }
    }
}

impl live::Document for Overlay {
    fn receipts(&mut self) -> &mut BTreeMap<String, String> {
        &mut self.receipts
    }

    fn answer(&mut self, receipt: &str) -> Result<Option<String>, String> {
        let path = std::path::Path::new(receipt);
        let mut answer = None;
        for (key, body) in &self.receipts {
            let old = std::path::Path::new(key);
            // Other cycles need no filesystem lookup; retired directories must
            // not prevent unrelated admission requests.
            if old.file_name() == path.file_name() && receipt_key(old)? == receipt_key(path)? {
                if answer.as_ref().is_some_and(|previous| previous != body) {
                    return Err("conflicting answers for the same admission receipt".into());
                }
                answer = Some(body.clone());
            }
        }
        Ok(answer)
    }
}

fn handed(state: &Overlay, repo: &str) -> Vec<Handed> {
    let prefix = format!("{}#", repo.to_ascii_lowercase());
    state
        .handoffs
        .iter()
        .filter(|(pr, _)| pr.starts_with(&prefix))
        .map(|(_, h)| h.clone())
        .collect()
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
        ["land", target, head] => handoff(target).is_some() && commit(head),
        ["landing", repo, owner] => {
            burrow_github::repo_name(repo) && landing::Owner::parse(owner).is_some()
        }
        ["pipeline", until] => until.parse::<i64>().is_ok_and(|t| t > now()),
        ["credit", rest @ ..] => credit::valid(rest),
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

    /// The configured landing owners as the overlay keeps them, for
    /// `proj settings show`.
    pub fn landing_set(&self) -> Result<BTreeMap<String, landing::Owner>, String> {
        Ok(self.read()?.landing)
    }

    fn read(&self) -> Result<Overlay, String> {
        live::read(&self.overlay)
    }

    /// Who holds landing now: every repo the steward's with no record kept.
    pub fn record(&self) -> Result<landing::Record, String> {
        self.landing_record
            .as_deref()
            .map_or(Ok(landing::Record::default()), landing::Record::read)
    }

    /// Each repo's configured landing owner other than the steward. One
    /// given to conaryd that's no longer among the repos it may have
    /// (`conaryd`) is the steward's again.
    pub fn landing_owners(&self) -> Result<BTreeMap<String, landing::Owner>, String> {
        let mut owners = self.read()?.landing;
        owners.retain(|repo, owner| *owner != landing::Owner::Conaryd || self.may_have(repo));
        Ok(owners)
    }

    fn may_have(&self, repo: &str) -> bool {
        self.conaryd.iter().any(|r| r.eq_ignore_ascii_case(repo))
    }

    /// Read again on every admission, including a long-lived reader's next job.
    pub fn allows(&self, repo: &str) -> Result<bool, String> {
        Ok(self.decision(repo)?.allowed)
    }

    pub fn decision(&self, repo: &str) -> Result<Decision, String> {
        Ok(self.snapshot()?.decision(repo))
    }

    /// Freeze both the overlay and scope clock for a durable batch operation.
    /// Ordinary admission still reloads the live policy on every decision.
    pub fn snapshot(&self) -> Result<Snapshot, String> {
        Ok(Snapshot {
            config: self.clone(),
            state: self.read()?,
            record: self.record()?,
            at: now(),
        })
    }

    /// Every PR handed to the steward, as `owner/repo#n` and its handoff.
    pub fn handoffs(&self) -> Result<BTreeMap<String, Handed>, String> {
        Ok(self.read()?.handoffs)
    }

    pub fn scope_until(&self) -> Result<Option<i64>, String> {
        Ok(self.read()?.pipeline_until.filter(|t| *t > now()))
    }

    fn permits(&self, state: &Overlay, repo: &str) -> bool {
        self.permits_at(state, repo, now())
    }

    fn permits_at(&self, state: &Overlay, repo: &str, at: i64) -> bool {
        let allowed = state
            .repos
            .get(&repo.to_ascii_lowercase())
            .copied()
            .unwrap_or_else(|| self.allowed.iter().any(|r| r.eq_ignore_ascii_case(repo)));
        allowed
            && (!state.pipeline_until.is_some_and(|t| t > at)
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
        let handed: Vec<_> = state
            .handoffs
            .iter()
            .map(|(pr, h)| format!("{pr} at {}", &h.head[..7]))
            .collect();
        let record = self.record()?;
        let mut landing: BTreeMap<String, String> = record
            .repos
            .iter()
            .filter(|(_, h)| h.owner != landing::Owner::Steward || h.to.is_some())
            .map(|(repo, h)| {
                let to = h.to.map_or(String::new(), |to| {
                    format!(", handing over to {}", to.name())
                });
                (repo.clone(), format!("{repo} by {}{to}", h.owner.name()))
            })
            .collect();
        for (repo, owner) in self.landing_owners()? {
            let h = record.holding(&repo);
            if h.owner != owner && h.to != Some(owner) {
                landing.insert(
                    repo.clone(),
                    format!("{repo} by {}, set to {}", h.owner.name(), owner.name()),
                );
            }
        }
        Ok(format!(
            "Automated work: {}; allowed: {}; paused: {}; all unlisted repos paused (including new repos){}{}\n",
            scoped.map_or("allow list".into(), |t| format!("pipeline-only until {t}")),
            allowed.join(", "),
            paused.join(", "),
            if handed.is_empty() {
                String::new()
            } else {
                format!("; handed to the steward: {}", handed.join(", "))
            },
            if landing.is_empty() {
                String::new()
            } else {
                format!(
                    "; landed by conaryd or handing over: {}; every other repo by the steward",
                    landing.into_values().collect::<Vec<_>>().join(", ")
                )
            }
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
        // Admission keys retain the supplied spelling for older generations.
        self.change(args, actor, Some(&receipt.to_string_lossy()))
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
        if let ["credit", ..] = words.as_slice() {
            let key = receipt
                .map(|r| receipt_key(std::path::Path::new(r)))
                .transpose()?;
            return self.change_credit(args, actor, key.as_deref());
        }
        if let ["role" | "model" | "offer" | "engines", ..] = words.as_slice() {
            let key = receipt
                .map(|r| receipt_key(std::path::Path::new(r)))
                .transpose()?;
            return self.change_roles(&words, args, actor, key.as_deref());
        }
        let answer = live::write(
            &self.overlay,
            &self.overlay.with_extension("lock"),
            &self.overlay.with_extension("log"),
            actor,
            receipt,
            args,
            |state: &mut Overlay| self.apply_overlay_command(state, &words, actor),
        )?;
        match answer {
            Some(said) if !said.is_empty() => Ok(said),
            _ if receipt.is_some() => {
                Ok("Settings changed; use settings show for the current policy.".into())
            }
            _ => self.show(),
        }
    }

    fn apply_overlay_command(
        &self,
        state: &mut Overlay,
        words: &[&str],
        actor: &str,
    ) -> Result<Option<String>, String> {
        match words {
            [verb @ ("allow" | "pause"), repo] if burrow_github::repo_name(repo) => {
                let repo = repo.to_ascii_lowercase();
                if *verb == "pause" {
                    // A pause is newer than any handoff in its repo.
                    let prefix = format!("{repo}#");
                    state.handoffs.retain(|pr, _| !pr.starts_with(&prefix));
                }
                state.repos.insert(repo, *verb == "allow");
            }
            ["land", target, head] if commit(head) => {
                let (key, repo, n) = handoff(target).ok_or("land needs OWNER/REPO#N")?;
                // A handoff is the steward landing a PR: never in a
                // repo it doesn't hold, or two would land there.
                if !self.record()?.holding(&repo).lands(landing::Owner::Steward) {
                    return Err(format!(
                        "{repo}'s landing isn't the steward's (settings show says whose): \
                         a handoff would have the steward land beside it"
                    ));
                }
                let head = head.to_ascii_lowercase();
                state.handoffs.insert(
                    key.clone(),
                    Handed {
                        pr: n,
                        head: head.clone(),
                        at: now(),
                        actor: actor.into(),
                    },
                );
                return Ok(Some(format!(
                    "Handed {key} to the steward at {}: it lands once that head passes every check, \
                     then the handoff ends. The rest of its repo stays as it is.",
                    &head[..7]
                )));
            }
            // The steward's: the handoff is used up (its PR landed or
            // closed). Only the head it was given, so a newer request
            // for another head stays.
            ["spent", target, head] => {
                let (key, _, _) = handoff(target).ok_or("spent needs OWNER/REPO#N")?;
                if state
                    .handoffs
                    .get(&key)
                    .is_none_or(|h| !h.head.eq_ignore_ascii_case(head))
                {
                    return Ok(None);
                }
                state.handoffs.remove(&key);
            }
            ["landing", repo, owner] if burrow_github::repo_name(repo) => {
                let owner = landing::Owner::parse(owner)
                    .ok_or("landing needs OWNER/REPO steward|conaryd")?;
                let repo = repo.to_ascii_lowercase();
                if owner == landing::Owner::Conaryd && !self.may_have(&repo) {
                    return Err(format!(
                        "conaryd has no flow configuration for {repo}: add it to burrow.landing.conaryd first"
                    ));
                }
                if owner == landing::Owner::Steward {
                    state.landing.remove(&repo);
                } else {
                    state.landing.insert(repo.clone(), owner);
                }
                return Ok(Some(format!(
                    "{repo}'s landing goes to the {}: the one holding it lets go of its work there first, \
                     and neither lands meanwhile. settings show says where it stands.",
                    if owner == landing::Owner::Conaryd {
                        "conaryd flow"
                    } else {
                        "steward"
                    }
                )));
            }
            ["pipeline", until] => {
                let t: i64 = until
                    .parse()
                    .map_err(|_| "pipeline needs a Unix timestamp")?;
                if t <= now() {
                    return Err("pipeline deadline must be in the future".into());
                }
                state.pipeline_until = Some(t);
            }
            ["back"] => state.pipeline_until = None,
            _ => {
                return Err(format!(
                    "usage: settings show | allow OWNER/REPO | pause OWNER/REPO | land OWNER/REPO#N HEAD | landing OWNER/REPO steward|conaryd | pipeline UNTIL | back
{}
{}",
                    roles::USAGE,
                    credit::USAGE
                ));
            }
        }
        Ok(Some(String::new()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
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
    fn words(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn a_handoff_admits_one_pr_at_one_head_until_spent_or_paused() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config {
            overlay: dir.path().join("overlay.json"),
            allowed: vec!["sam/open".into()],
            ..Default::default()
        };
        let (a, b) = ("a".repeat(40), "b".repeat(40));
        for bad in [
            format!("land sam/box {a}"),
            format!("land sam/box#0 {a}"),
            format!("land sam/box#07 {a}"),
            format!("land sam/box#7 {}", &a[..12]),
            format!("land sam/box#7 {}", "z".repeat(40)),
            format!("land ../box#7 {a}"),
        ] {
            assert!(!valid_command(&words(&bad)), "{bad}");
            assert!(cfg.command(&words(&bad), "host").is_err(), "{bad}");
        }
        assert!(valid_command(&words(&format!("land Sam/Box#7 {a}"))));
        assert!(
            !valid_command(&words(&format!("spent sam/box#7 {a}"))),
            "spending is the steward's, never the door's"
        );
        let said = cfg
            .command(&words(&format!("land Sam/Box#7 {a}")), "dev")
            .unwrap();
        assert!(said.contains("sam/box#7"), "{said}");
        let d = cfg.decision("sam/box").unwrap();
        assert!(!d.allowed && !d.vacate, "the repo itself stays paused");
        assert_eq!(d.handed.len(), 1);
        assert_eq!((d.handed[0].pr, d.handed[0].head.as_str()), (7, a.as_str()));
        assert_eq!(d.handed[0].actor, "dev");
        assert!(cfg.decision("sam/boxer").unwrap().handed.is_empty());
        assert!(cfg.decision("sam/open").unwrap().handed.is_empty());
        assert!(
            cfg.show()
                .unwrap()
                .contains("handed to the steward: sam/box#7 at aaaaaaa")
        );
        // A newer request for the same PR moves it to that head only.
        cfg.command(&words(&format!("land sam/box#7 {b}")), "dev")
            .unwrap();
        assert_eq!(cfg.decision("sam/box").unwrap().handed[0].head, b);
        // Spending the old head leaves the newer request.
        cfg.command(&words(&format!("spent sam/box#7 {a}")), "steward")
            .unwrap();
        assert_eq!(cfg.decision("sam/box").unwrap().handed.len(), 1);
        cfg.command(&words(&format!("spent sam/box#7 {b}")), "steward")
            .unwrap();
        assert!(cfg.decision("sam/box").unwrap().handed.is_empty());
        assert!(
            !fs::read_to_string(&cfg.overlay)
                .unwrap()
                .contains("handoffs"),
            "an overlay with none reads as an older generation wrote it"
        );
        // A pause, newer than the handoff, revokes it; other repos' stay.
        cfg.command(&words(&format!("land sam/box#7 {a}")), "dev")
            .unwrap();
        cfg.command(&words(&format!("land sam/other#1 {a}")), "dev")
            .unwrap();
        cfg.command(&words("pause sam/box"), "host").unwrap();
        assert!(cfg.decision("sam/box").unwrap().handed.is_empty());
        assert_eq!(cfg.decision("sam/other").unwrap().handed.len(), 1);
    }

    #[test]
    fn a_redelivered_handoff_cannot_come_back_once_spent() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config {
            overlay: dir.path().join("overlay.json"),
            ..Default::default()
        };
        let head = "c".repeat(40);
        let receipt = dir.path().join("issue-open-cycle");
        let first = cfg
            .request(
                &receipt,
                &words(&format!("land sam/box#3 {head}")),
                "github",
            )
            .unwrap();
        cfg.command(&words(&format!("spent sam/box#3 {head}")), "steward")
            .unwrap();
        // The same delivery again, as after a restart: the answer, no handoff.
        let again = cfg
            .request(
                &receipt,
                &words(&format!("land sam/box#3 {head}")),
                "github",
            )
            .unwrap();
        assert_eq!(first, again);
        assert!(cfg.decision("sam/box").unwrap().handed.is_empty());
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

pub mod reference;
pub mod schema;
pub mod show;

#[cfg(test)]
#[path = "admission_receipts_tests.rs"]
mod admission_receipts;
