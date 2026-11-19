//! Installation quota evidence shared by the gh door and proj. No tokens or
//! request bodies are kept. An installation is identified by App and owner.
use crate::{Response, graphql_rate_error, rate_limited, retry_at};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{io, path::Path};

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
        // Hex encodes the two components without aliases or path traversal.
        let key = format!("{}\0{}", self.app, self.owner)
            .bytes()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
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
