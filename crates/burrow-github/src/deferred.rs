//! Durable rate-limit deferral for serialized issue-door passes.
use crate::{Api, Failed, Response};
use serde_json::{Value, json};
use std::{fs, path::Path};

/// The issue-door services configure their own wake file. Local commands do
/// not, so a transient failure cannot silently become success without a retry.
pub fn configured<T>(
    pass: impl FnOnce(&mut dyn Api) -> Result<T, String>,
) -> Result<Option<T>, String> {
    let wake = std::env::var_os("BURROW_DOOR_WAKE").map(std::path::PathBuf::from);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();
    run(wake.as_deref(), &mut crate::Gh::default(), now, pass)
}

/// Run a pass, preserving a deferred full pass in burrow-events' wake file.
/// Without a wake file, errors retain their usual meaning.
pub fn run<T>(
    wake: Option<&Path>,
    api: &mut dyn Api,
    now: u64,
    pass: impl FnOnce(&mut dyn Api) -> Result<T, String>,
) -> Result<Option<T>, String> {
    let Some(wake) = wake else {
        return pass(api).map(Some);
    };
    let booked = match fs::read(wake) {
        Ok(bytes) => {
            let value: Value =
                serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", wake.display()))?;
            let object = value
                .as_object()
                .ok_or_else(|| format!("{}: expected wake object", wake.display()))?;
            match object.get("") {
                Some(v) => Some(
                    v.as_u64()
                        .ok_or_else(|| format!("{}: invalid wake time", wake.display()))?,
                ),
                None => None,
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("{}: {e}", wake.display())),
    };
    if booked.is_some_and(|at| at > now) {
        return Ok(None);
    }
    let mut api = Limited {
        inner: api,
        now,
        limited: Vec::new(),
    };
    match pass(&mut api) {
        Ok(done) => {
            save(wake, None)?;
            Ok(Some(done))
        }
        Err(e) => {
            // Defer only the exact API error that ended the pass. An error
            // caught by the pass must not conceal a later, unrelated failure.
            let at = api
                .limited
                .iter()
                .filter(|(why, _)| why == &e)
                .map(|(_, at)| *at)
                .max();
            match at {
                Some(at) => {
                    save(wake, Some(at))?;
                    eprintln!("GitHub rate limited this pass; pending until {at}: {e}");
                    Ok(None)
                }
                None => {
                    // The due booking has been consumed. Genuine failures use
                    // burrow-events' failure backoff, not a stale past wake.
                    if booked.is_some() {
                        save(wake, None)?;
                    }
                    Err(e)
                }
            }
        }
    }
}

fn save(path: &Path, at: Option<u64>) -> Result<(), String> {
    let write = || -> std::io::Result<()> {
        use std::io::Write;
        let temp = path.with_extension("new");
        let mut file = fs::File::create(&temp)?;
        file.write_all(
            at.map_or_else(|| json!({}), |at| json!({"": at}))
                .to_string()
                .as_bytes(),
        )?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        fs::File::open(path.parent().unwrap_or(Path::new(".")))?.sync_all()
    };
    write().map_err(|e| format!("{}: {e}", path.display()))
}

struct Limited<'a> {
    inner: &'a mut dyn Api,
    now: u64,
    limited: Vec<(String, u64)>,
}
impl Limited<'_> {
    fn note(&mut self, what: &str, response: &Response) {
        if !super::rate_limited(response) {
            return;
        }
        let retry = response
            .header("retry-after")
            .and_then(|v| v.parse::<u64>().ok())
            .map(|s| self.now.saturating_add(s));
        let reset = (response.header("x-ratelimit-remaining") == Some("0"))
            .then(|| {
                response
                    .header("x-ratelimit-reset")
                    .and_then(|v| v.parse::<u64>().ok())
            })
            .flatten();
        let at = retry
            .into_iter()
            .chain(reset)
            .filter(|at| *at > self.now)
            .max()
            .unwrap_or(self.now.saturating_add(60));
        if let Err(e) = super::kept(what, response.clone(), &[]) {
            self.limited.push((e.why, at));
        }
        // call's bounded 429 retries use this diagnostic after three tries.
        if response.status == 429 {
            self.limited.push((
                format!("{what}: HTTP 429, {} times", super::AGAIN.len() + 1),
                at,
            ));
        }
    }
}
impl Api for Limited<'_> {
    fn request(
        &mut self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        etag: Option<&str>,
    ) -> Result<Response, Failed> {
        let response = self.inner.request(method, path, body, etag)?;
        self.note(&format!("{method} {path}"), &response);
        Ok(response)
    }
    fn graphql(&mut self, owner: &str, body: &Value) -> Result<Response, Failed> {
        let response = self.inner.graphql(owner, body)?;
        self.note("POST graphql", &response);
        Ok(response)
    }
    fn pause(&mut self, secs: u64) {
        self.inner.pause(secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::call;
    struct Hub {
        status: u16,
        headers: Vec<(String, String)>,
        calls: usize,
    }
    impl Api for Hub {
        fn request(
            &mut self,
            _: &str,
            _: &str,
            _: Option<&Value>,
            _: Option<&str>,
        ) -> Result<Response, Failed> {
            self.calls += 1;
            Ok(Response {
                status: self.status,
                headers: self.headers.clone(),
                body: json!({"message":"denied"}).to_string(),
            })
        }
        fn pause(&mut self, _: u64) {}
    }
    fn hub(status: u16, headers: &[(&str, &str)]) -> Hub {
        Hub {
            status,
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            calls: 0,
        }
    }
    fn ask(api: &mut dyn Api) -> Result<(), String> {
        call(api, "GET", "repos/sam/r/issues", None, None)
            .map(|_| ())
            .map_err(Into::into)
    }
    #[test]
    fn exhausted_pass_is_durable_and_early_events_do_not_spend_requests() {
        let dir = tempfile::tempdir().unwrap();
        let wake = dir.path().join("wake.json");
        let mut api = hub(
            403,
            &[
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset", "2000"),
            ],
        );
        assert_eq!(run(Some(&wake), &mut api, 1000, ask), Ok(None));
        assert_eq!(fs::read_to_string(&wake).unwrap(), r#"{"":2000}"#);
        assert_eq!(run(Some(&wake), &mut api, 1500, ask), Ok(None));
        assert_eq!(api.calls, 1);
        api.status = 200;
        assert_eq!(run(Some(&wake), &mut api, 2000, ask), Ok(Some(())));
        assert_eq!(fs::read_to_string(&wake).unwrap(), "{}");
    }
    #[test]
    fn permission_denial_standalone_and_unrelated_failure_remain_failures() {
        let dir = tempfile::tempdir().unwrap();
        let wake = dir.path().join("wake.json");
        assert!(run(Some(&wake), &mut hub(403, &[]), 1000, ask).is_err());
        assert!(!wake.exists());
        assert!(run(None, &mut hub(429, &[]), 1000, ask).is_err());
        let mut api = hub(403, &[("retry-after", "90")]);
        let result: Result<Option<()>, String> = run(Some(&wake), &mut api, 1000, |api| {
            let _ = ask(api);
            Err("disk failure".into())
        });
        assert_eq!(result, Err("disk failure".into()));
    }
    #[test]
    fn secondary_limits_and_missing_reset_book_a_retry() {
        for (status, headers, at) in [
            (429, vec![("retry-after", "90")], 1090),
            (403, vec![("x-ratelimit-remaining", "0")], 1060),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let wake = dir.path().join("wake.json");
            assert_eq!(
                run(Some(&wake), &mut hub(status, &headers), 1000, ask),
                Ok(None)
            );
            let booked: Value = serde_json::from_slice(&fs::read(wake).unwrap()).unwrap();
            assert_eq!(booked[""], at);
        }
    }
    #[test]
    fn a_due_retry_that_hits_permission_denial_uses_failure_backoff() {
        let dir = tempfile::tempdir().unwrap();
        let wake = dir.path().join("wake.json");
        fs::write(&wake, r#"{"":1000}"#).unwrap();
        assert!(run(Some(&wake), &mut hub(403, &[]), 1000, ask).is_err());
        assert_eq!(fs::read_to_string(wake).unwrap(), "{}");
    }

    #[test]
    fn failed_persistence_is_not_a_successful_deferral() {
        let dir = tempfile::tempdir().unwrap();
        let wake = dir.path().join("absent/wake.json");
        assert!(
            run(
                Some(&wake),
                &mut hub(403, &[("retry-after", "90")]),
                1000,
                ask
            )
            .is_err()
        );
    }
}
