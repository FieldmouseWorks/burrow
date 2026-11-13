//! Installation quota evidence shared by the gh door, proj and every pass
//! that asks GitHub. No tokens or request bodies are kept. An installation
//! is identified by App and owner.
//!
//! The one rule: a pass asks its installation's meter before it starts
//! ([`Meters::held`]), feeds it every answer ([`Metered`]), and when GitHub
//! holds it off gets back one [`Hold`] (which installation, until when) to
//! book. What a limit looks like (a 403 or 429 and its headers, a GraphQL
//! error, `rate_limit`'s budgets) is read here and nowhere else.
use crate::{Api, Error, Failed, Response, graphql_rate_error, rate_limited, retry_at};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
};

/// One installation GitHub holds off until a time: the deferral every
/// caller books, whichever of them met it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hold {
    pub app: String,
    pub owner: String,
    /// Unix seconds: when the installation may be asked again.
    pub until: u64,
}

impl Hold {
    /// The hold E, from a call as APP on OWNER's installation, says that
    /// installation is under: when GitHub held off every cause of it.
    pub fn from_error(app: &str, owner: &str, e: &Error) -> Option<Self> {
        e.retry_at.map(|until| Self {
            app: app.into(),
            owner: owner.to_ascii_lowercase(),
            until,
        })
    }
}

/// Whether GitHub's answer is one of its limits (a 429, or a 403 its
/// headers or message say is one): for a client that reads GitHub's answers
/// itself, as gh-token's App does, instead of through [`crate::call`].
pub fn limited(response: &Response) -> bool {
    rate_limited(response)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Meter {
    pub app: String,
    pub owner: String,
    pub at: u64,
    pub remaining: Option<u64>,
    pub reset: Option<u64>,
    /// Secondary limits can hold even when the primary budget has room.
    pub retry_at: Option<u64>,
}
impl Meter {
    pub fn new(app: &str, owner: &str, at: u64) -> Self {
        Self {
            app: app.into(),
            owner: owner.to_ascii_lowercase(),
            at,
            remaining: None,
            reset: None,
            retry_at: None,
        }
    }
    pub fn deadline(&self) -> Option<u64> {
        self.retry_at
            .into_iter()
            .chain((self.remaining == Some(0)).then_some(self.reset).flatten())
            .max()
    }
    pub fn until(&self, at: u64) -> Option<u64> {
        self.deadline().filter(|t| *t > at)
    }
    /// The installation's hold at AT, if one stands.
    pub fn hold(&self, at: u64) -> Option<Hold> {
        self.until(at).map(|until| Hold {
            app: self.app.clone(),
            owner: self.owner.clone(),
            until,
        })
    }
    /// One answer that arrived at AT: its budget headers, and any limit it
    /// says. The newer budget stands; a hold only ever grows.
    pub fn observe(&mut self, response: &Response, at: u64) {
        let mut seen = Meter::new(&self.app, &self.owner, at);
        seen.remaining = response
            .header("x-ratelimit-remaining")
            .and_then(|s| s.parse().ok());
        seen.reset = response
            .header("x-ratelimit-reset")
            .and_then(|s| s.parse().ok());
        seen.response(response);
        self.merge(seen);
    }
    /// OTHER's reading of the same installation folded in: the newer budget
    /// (one with a budget over one without), and the later of their holds.
    fn merge(&mut self, other: Meter) {
        let retry_at = self.retry_at.max(other.retry_at);
        if (other.remaining.is_some(), other.at) > (self.remaining.is_some(), self.at) {
            self.at = other.at;
            self.remaining = other.remaining;
            self.reset = other.reset;
        }
        self.retry_at = retry_at;
    }
    /// The file APP's meter on OWNER's installation is kept in, in DIR.
    pub fn file(dir: &Path, app: &str, owner: &str) -> PathBuf {
        dir.join(format!("{}.json", key(app, &owner.to_ascii_lowercase())))
    }
    /// GET /rate_limit is free and reports both REST and GraphQL budgets.
    pub fn budget(&mut self, body: &Value) {
        let core = &body["resources"]["core"];
        self.remaining = core["remaining"].as_u64();
        self.reset = core["reset"].as_u64();
        let graphql = &body["resources"]["graphql"];
        if graphql["remaining"].as_u64() == Some(0) {
            self.retry_at = self
                .retry_at
                .into_iter()
                .chain(graphql["reset"].as_u64())
                .max();
        }
    }
    pub fn response(&mut self, response: &Response) {
        let graphql = serde_json::from_str::<Value>(&response.body)
            .ok()
            .and_then(|v| v["errors"].as_array().cloned())
            .is_some_and(|errors| !errors.is_empty() && errors.iter().all(graphql_rate_error));
        if rate_limited(response) || graphql {
            self.retry_at = Some(retry_at(response, self.at));
        }
    }
    pub fn read(file: &Path) -> Option<Self> {
        Self::try_read(file).ok().flatten()
    }
    pub fn try_read(file: &Path) -> io::Result<Option<Self>> {
        match burrow_process::state_reads::bytes(file) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
    pub fn record(&self, dir: &Path) -> io::Result<()> {
        self.record_visibility(dir, false)
    }
    /// Public budget facts, atomically readable by host schedulers.
    pub fn record_public(&self, dir: &Path) -> io::Result<()> {
        self.record_visibility(dir, true)
    }
    fn record_visibility(&self, dir: &Path, public: bool) -> io::Result<()> {
        burrow_process::state_reads::directory(dir)?;
        let key = key(&self.app, &self.owner);
        let lock = burrow_process::state_reads::append(&dir.join(format!("{key}.lock")))?;
        lock.lock()?;
        let file = dir.join(format!("{key}.json"));
        let mut next = self.clone();
        if let Some(old) = Self::try_read(&file)? {
            let hold = self
                .retry_at
                .into_iter()
                .chain(old.retry_at)
                .chain(self.until(self.at))
                .chain(old.until(self.at))
                .max();
            // Hold-only observations do not refresh the budget's age.
            if (old.remaining.is_some(), old.at) > (self.remaining.is_some(), self.at) {
                next = old;
            }
            next.retry_at = hold;
        }
        let bytes = serde_json::to_vec(&next)?;
        if public {
            burrow_process::state_reads::write_public(&file, bytes)
        } else {
            burrow_process::state_reads::write(&file, bytes)
        }
    }
}

/// Hex encodes the two components without aliases or path traversal.
fn key(app: &str, owner: &str) -> String {
    format!("{app}\0{owner}")
        .bytes()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Where an App's installations' meters are kept, as a program's config
/// names them: the gh door's dir (burrow.github.quotaDir) and the App's
/// name there.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Kept {
    pub dir: PathBuf,
    pub app: String,
}

impl Kept {
    /// The meters KEPT names, or this process's own when there are none.
    pub fn meters(kept: Option<&Kept>) -> Meters {
        match kept {
            Some(k) => Meters::new(Some(k.dir.clone()), &k.app),
            None => Meters::new(None, ""),
        }
    }
}

/// An App's installations' meters, as one process sees them: the shared
/// ones in `dir` (the gh door's, which every caller reads) and what this
/// process met itself, which it records there when it may.
#[derive(Debug)]
pub struct Meters {
    dir: Option<PathBuf>,
    app: String,
    seen: BTreeMap<String, Meter>,
    /// Whether a failure to record has been said: once a process, not per
    /// call.
    unrecorded: bool,
}

impl Meters {
    /// APP's meters, shared in DIR when there is one, else this process's
    /// alone.
    pub fn new(dir: Option<PathBuf>, app: &str) -> Self {
        Self {
            dir,
            app: app.into(),
            seen: BTreeMap::new(),
            unrecorded: false,
        }
    }

    pub fn app(&self) -> &str {
        &self.app
    }

    /// OWNER's installation's meter, with the shared one read in afresh.
    fn meter(&mut self, owner: &str) -> &mut Meter {
        let owner = owner.to_ascii_lowercase();
        let shared = self
            .dir
            .as_deref()
            .and_then(|dir| Meter::read(&Meter::file(dir, &self.app, &owner)))
            .filter(|m| m.app == self.app && m.owner == owner);
        let meter = self
            .seen
            .entry(owner.clone())
            .or_insert_with(|| Meter::new(&self.app, &owner, 0));
        if let Some(shared) = shared {
            meter.merge(shared);
        }
        meter
    }

    /// The hold on OWNER's installation at NOW, whoever met it: a pass that
    /// gets one asks GitHub nothing more there, and books it.
    pub fn held(&mut self, owner: &str, now: u64) -> Option<Hold> {
        self.meter(owner).hold(now)
    }

    /// Every hold this process has seen that still stands at NOW.
    pub fn holds(&self, now: u64) -> Vec<Hold> {
        self.seen.values().filter_map(|m| m.hold(now)).collect()
    }

    /// One answer about OWNER's installation, arrived at AT. A limit is
    /// recorded at once, so a caller starting next finds it.
    pub fn observe(&mut self, owner: &str, response: &Response, at: u64) {
        let meter = self.meter(owner);
        let before = meter.until(at);
        meter.observe(response, at);
        if meter.until(at) > before {
            self.record_one(owner);
        }
    }

    /// E, from a pass on OWNER's installation, as of NOW: its hold when E is
    /// GitHub's limit (every cause one), recorded for the next caller.
    pub fn met(&mut self, owner: &str, e: &Error, now: u64) -> Option<Hold> {
        let met = Hold::from_error(&self.app, owner, e)?;
        Some(self.hold(owner, met.until, now))
    }

    /// A hold met on OWNER's installation until UNTIL, as of NOW, recorded
    /// for the next caller: the installation's hold with it (which can be
    /// later, when the meter knows of a later one).
    pub fn hold(&mut self, owner: &str, until: u64, now: u64) -> Hold {
        let meter = self.meter(owner);
        meter.retry_at = meter.retry_at.max(Some(until));
        if meter.at == 0 {
            meter.at = now;
        }
        let hold = meter.hold(now).unwrap_or_else(|| Hold {
            app: meter.app.clone(),
            owner: meter.owner.clone(),
            until,
        });
        self.record_one(owner);
        hold
    }

    /// After a run of another program as OWNER's installation failed (one
    /// that can't feed the meter call by call, as Renovate can't): its hold,
    /// when `rate_limit` (which costs nothing against the allowance) says
    /// the installation's budget is spent. None when it isn't, or GitHub
    /// won't say: then the run failed.
    pub fn spent(&mut self, api: &mut dyn Api, owner: &str) -> Option<Hold> {
        let budget = crate::call(api, "GET", "rate_limit", None, None)
            .ok()?
            .json()
            .ok()?;
        let at = api.now();
        let mut read = Meter::new(&self.app, owner, at);
        read.budget(&budget);
        let meter = self.meter(owner);
        meter.merge(read.clone());
        meter.retry_at = meter.retry_at.max(read.until(at));
        let hold = meter.hold(at);
        self.record_one(owner);
        hold
    }

    /// Record every meter this process has seen in the shared dir, as far as
    /// it may (Meters::unrecorded).
    pub fn record(&mut self) {
        let owners: Vec<String> = self.seen.keys().cloned().collect();
        for owner in owners {
            self.record_one(&owner);
        }
    }

    fn record_one(&mut self, owner: &str) {
        let Some(dir) = &self.dir else { return };
        let Some(meter) = self.seen.get(&owner.to_ascii_lowercase()) else {
            return;
        };
        if meter.at == 0 && meter.retry_at.is_none() {
            return;
        }
        // The shared dir is the gh door's (burrow.github.quotaDir), which
        // records what it sees itself: a caller it doesn't let write there
        // keeps what it met to itself. Anything else is said, once.
        if let Err(e) = meter.record_public(dir)
            && e.kind() != io::ErrorKind::PermissionDenied
            && !std::mem::replace(&mut self.unrecorded, true)
        {
            eprintln!(
                "burrow-github: couldn't record {}'s meter on {} in {}: {e}",
                self.app,
                meter.owner,
                dir.display()
            );
        }
    }
}

/// An Api whose every answer feeds the meter of the installation it was
/// about: a GraphQL call's owner, a REST path's `repos/OWNER/...`, else
/// `owner` (the installation a client bound to one account is about).
pub struct Metered<'a> {
    pub api: &'a mut dyn Api,
    pub meters: &'a mut Meters,
    pub owner: Option<String>,
}

impl Metered<'_> {
    fn feed(&mut self, owner: Option<&str>, got: &Result<Response, Failed>) {
        let Ok(response) = got else { return };
        let Some(owner) = owner.or(self.owner.as_deref()) else {
            return;
        };
        let owner = owner.to_owned();
        let at = self.api.now();
        self.meters.observe(&owner, response, at);
    }
}

/// The owner a REST path names: `repos/OWNER/...`.
fn owner_of(path: &str) -> Option<&str> {
    let path = path.trim_start_matches('/');
    let path = path.strip_prefix("https://api.github.com/").unwrap_or(path);
    let mut parts = path.split(['/', '?']);
    match (parts.next(), parts.next()) {
        (Some("repos"), Some(owner)) if !owner.is_empty() => Some(owner),
        _ => None,
    }
}

impl Api for Metered<'_> {
    fn request(
        &mut self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        etag: Option<&str>,
    ) -> Result<Response, Failed> {
        let got = self.api.request(method, path, body, etag);
        self.feed(owner_of(path), &got);
        got
    }

    fn graphql(&mut self, owner: &str, body: &Value) -> Result<Response, Failed> {
        let got = self.api.graphql(owner, body);
        self.feed(Some(owner), &got);
        got
    }

    fn now(&self) -> u64 {
        self.api.now()
    }

    fn pause(&mut self, secs: u64) {
        self.api.pause(secs);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn same_limit_evidence_as_the_shared_client_and_no_permission_denials() {
        for (status, headers, body, expected) in [
            (429, vec![], "{}", Some(1060)),
            (
                403,
                vec![
                    ("x-ratelimit-remaining", "0"),
                    ("x-ratelimit-reset", "2000"),
                ],
                "{}",
                Some(2000),
            ),
            (403, vec![("retry-after", "120")], "{}", Some(1120)),
            (
                403,
                vec![],
                r#"{"message":"API rate limit exceeded"}"#,
                Some(1060),
            ),
            (
                403,
                vec![],
                r#"{"message":"exceeded a secondary rate limit"}"#,
                Some(1060),
            ),
            (
                200,
                vec![],
                r#"{"errors":[{"type":"RATE_LIMITED"}]}"#,
                Some(1060),
            ),
            (
                200,
                vec![],
                r#"{"errors":[{"extensions":{"code":"RATE_LIMITED"}}]}"#,
                Some(1060),
            ),
            (
                200,
                vec![],
                r#"{"errors":[{"type":"RATE_LIMITED"},{"type":"FORBIDDEN"}]}"#,
                None,
            ),
            (
                403,
                vec![],
                r#"{"message":"Resource not accessible by integration"}"#,
                None,
            ),
        ] {
            let mut meter = Meter::new("workers", "sam", 1000);
            meter.response(&Response {
                status,
                headers: headers
                    .into_iter()
                    .map(|(k, v)| (k.into(), v.into()))
                    .collect(),
                body: body.into(),
            });
            assert_eq!(meter.retry_at, expected, "{status} {body}");
        }
    }
    #[test]
    fn free_budget_read_and_secondary_hold_are_independent() {
        let d = tempfile::tempdir().unwrap();
        let mut meter = Meter::new("workers", "Sam", 1000);
        meter.budget(&json!({"resources":{"core":{"remaining":3500,"reset":5000}}}));
        meter.retry_at = Some(1300);
        meter.record(d.path()).unwrap();
        let mut newer = Meter::new("workers", "sam", 1100);
        newer.budget(&json!({"resources":{"core":{"remaining":3400,"reset":5000}}}));
        newer.record(d.path()).unwrap();
        let file = std::fs::read_dir(d.path())
            .unwrap()
            .filter_map(Result::ok)
            .find(|f| f.path().extension().is_some_and(|s| s == "json"))
            .unwrap()
            .path();
        let m = Meter::read(&file).unwrap();
        assert_eq!(m.remaining, Some(3400));
        assert_eq!(m.owner, "sam");
        assert_eq!(m.until(1200), Some(1300));
        assert_eq!(m.until(1300), None);
    }
}

#[cfg(test)]
mod ordering_tests {
    use super::*;
    #[test]
    fn delayed_refusals_and_concurrent_writers_cannot_shrink_holds() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path();
        let mut fresh = Meter::new("app", "Sam", 1100);
        fresh.remaining = Some(3500);
        fresh.reset = Some(5000);
        fresh.record(dir).unwrap();
        let mut delayed = Meter::new("app", "sam", 1000);
        delayed.retry_at = Some(3000);
        delayed.record(dir).unwrap();
        let file = || {
            std::fs::read_dir(dir)
                .unwrap()
                .filter_map(Result::ok)
                .find(|f| f.path().extension().is_some_and(|s| s == "json"))
                .unwrap()
                .path()
        };
        let m = Meter::read(&file()).unwrap();
        assert_eq!(
            (m.at, m.remaining, m.retry_at),
            (1100, Some(3500), Some(3000))
        );
        std::thread::scope(|scope| {
            for i in 0..16 {
                scope.spawn(move || {
                    let mut m = Meter::new("app", "SAM", 1200 + i);
                    m.remaining = Some(100 + i);
                    m.retry_at = Some(4000 + i);
                    m.record(dir).unwrap();
                });
            }
        });
        let m = Meter::read(&file()).unwrap();
        assert_eq!(
            (m.at, m.remaining, m.retry_at),
            (1215, Some(115), Some(4015))
        );
    }
}

#[cfg(test)]
mod delayed_sample_test {
    use super::*;
    use serde_json::json;
    #[test]
    fn a_late_older_sample_cannot_shrink_a_live_hold() {
        let d = tempfile::tempdir().unwrap();
        let mut newer = Meter::new("workers", "sam", 1100);
        newer.budget(&json!({"resources":{"core":{"remaining":3400,"reset":5000}}}));
        newer.record(d.path()).unwrap();
        let mut older = Meter::new("workers", "sam", 1000);
        older.retry_at = Some(1500);
        older.record(d.path()).unwrap();
        let file = std::fs::read_dir(d.path())
            .unwrap()
            .filter_map(Result::ok)
            .find(|f| f.path().extension().is_some_and(|s| s == "json"))
            .unwrap()
            .path();
        let m = Meter::read(&file).unwrap();
        assert_eq!((m.at, m.remaining), (1100, Some(3400)));
        assert_eq!(m.until(1200), Some(1500));
    }
}

#[cfg(test)]
mod storage_failures {
    use super::*;
    #[test]
    fn unreadable_prior_state_is_never_replaced_as_absent() {
        let d = tempfile::tempdir().unwrap();
        let mut m = Meter::new("app", "owner", 1000);
        m.retry_at = Some(4000);
        m.record(d.path()).unwrap();
        let file = std::fs::read_dir(d.path())
            .unwrap()
            .filter_map(Result::ok)
            .find(|e| e.path().extension().is_some_and(|s| s == "json"))
            .unwrap()
            .path();
        std::fs::remove_file(&file).unwrap();
        std::fs::create_dir(&file).unwrap();
        m.at = 2000;
        m.retry_at = None;
        assert!(m.record(d.path()).is_err());
        assert!(file.is_dir());
    }
}

#[cfg(test)]
mod public_records {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn host_budget_records_are_atomically_public_on_every_replacement() {
        let d = tempfile::tempdir().unwrap();
        let mut m = Meter::new("app", "owner", 1000);
        for at in [1000, 2000] {
            m.at = at;
            m.record_public(d.path()).unwrap();
            let file = std::fs::read_dir(d.path())
                .unwrap()
                .filter_map(Result::ok)
                .find(|e| e.path().extension().is_some_and(|s| s == "json"))
                .unwrap()
                .path();
            assert_eq!(
                std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
                0o644
            );
            assert_eq!(Meter::try_read(&file).unwrap().unwrap().at, at);
        }
    }
}
