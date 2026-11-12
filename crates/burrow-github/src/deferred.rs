//! Durable rate-limit deferral: a pass GitHub held off, booked again in its
//! burrow-events wake file for when the limit lifts.
use crate::{Api, Error};
use serde_json::{Map, Value, json};
use std::{fs, path::Path};

/// The issue-door services configure their own wake file. Local commands do
/// not, so a transient failure cannot silently become success without a retry.
pub fn configured<T>(
    pass: impl FnOnce(&mut dyn Api) -> Result<T, Error>,
) -> Result<Option<T>, String> {
    let wake = std::env::var_os("BURROW_DOOR_WAKE").map(std::path::PathBuf::from);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();
    run(wake.as_deref(), &mut crate::Gh::default(), now, pass).map_err(Into::into)
}

/// Run a pass, preserving a deferred full pass in burrow-events' wake file.
/// Without a wake file, errors retain their usual meaning.
pub fn run<T>(
    wake: Option<&Path>,
    api: &mut dyn Api,
    now: u64,
    pass: impl FnOnce(&mut dyn Api) -> Result<T, Error>,
) -> Result<Option<T>, Error> {
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
        Err(e) => return Err(format!("{}: {e}", wake.display()).into()),
    };
    if booked.is_some_and(|at| at > now) {
        return Ok(None);
    }
    match pass(api) {
        Ok(done) => {
            save(wake, None)?;
            Ok(Some(done))
        }
        Err(e) => {
            match e.retry_at {
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

/// Book KEY's pass (an OWNER/REPO, or "" for the full pass) for AT in the
/// wake file, keeping every other booking: for a program whose passes over
/// one repo and over every repo can run side by side (renovate-run), so
/// each change is made under the wake file's lock. A later booking already
/// there stands. Keys are kept in lower case, as burrow-events reads them.
pub fn book(wake: &Path, key: &str, at: u64) -> Result<(), String> {
    keyed(wake, |booked| {
        let key = key.to_lowercase();
        let later = booked
            .get(&key)
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .max(at);
        booked.insert(key, later.into());
    })
}

/// Book KEY's pass for AT in place of any booking for it, keeping every
/// other key's: for a pass that has just found out when its own wait ends,
/// so neither a later booking from an earlier pass nor a due one stands in
/// its way.
pub fn rebook(wake: &Path, key: &str, at: u64) -> Result<(), String> {
    keyed(wake, |booked| {
        booked.insert(key.to_lowercase(), at.into());
    })
}

/// KEY's pass ran, as of NOW: a booking for it that had come due is used up,
/// and one still to come stands.
pub fn ran(wake: &Path, key: &str, now: u64) -> Result<(), String> {
    keyed(wake, |booked| {
        let key = key.to_lowercase();
        if booked
            .get(&key)
            .is_some_and(|at| at.as_u64().is_none_or(|at| at <= now))
        {
            booked.remove(&key);
        }
    })
}

/// Change the wake file's bookings with CHANGE, holding the lock beside it
/// from the read to the write. A missing file is no bookings.
fn keyed(wake: &Path, change: impl FnOnce(&mut Map<String, Value>)) -> Result<(), String> {
    let lock = wake.with_extension("lock");
    let held = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock)
        .and_then(|f| f.lock().map(|()| f))
        .map_err(|e| format!("{}: {e}", lock.display()))?;
    let mut booked = match fs::read(wake) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(Value::Object(booked)) => booked,
            _ => return Err(format!("{}: expected wake object", wake.display())),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Map::new(),
        Err(e) => return Err(format!("{}: {e}", wake.display())),
    };
    change(&mut booked);
    let saved = write(wake, &Value::Object(booked));
    drop(held);
    saved
}

fn save(path: &Path, at: Option<u64>) -> Result<(), String> {
    write(path, &at.map_or_else(|| json!({}), |at| json!({"": at})))
}

fn write(path: &Path, booked: &Value) -> Result<(), String> {
    let write = || -> std::io::Result<()> {
        // The scheduler is a different account, even if a caller has a
        // private umask. This file contains only the public retry deadline.
        burrow_process::replace::replace_mode(
            path,
            booked.to_string(),
            burrow_process::replace::SHARED,
        )
    };
    write().map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Failed, Response, call};
    struct Hub {
        status: u16,
        headers: Vec<(String, String)>,
        calls: usize,
        clock: u64,
        latency: u64,
        body: Value,
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
            self.clock += self.latency;
            Ok(Response {
                status: self.status,
                headers: self.headers.clone(),
                body: self.body.to_string(),
            })
        }
        fn now(&self) -> u64 {
            self.clock
        }
        fn pause(&mut self, secs: u64) {
            self.clock += secs;
        }
    }
    fn hub(status: u16, headers: &[(&str, &str)]) -> Hub {
        Hub {
            status,
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            calls: 0,
            clock: 1000,
            latency: 0,
            body: json!({"message":"denied"}),
        }
    }
    fn ask(api: &mut dyn Api) -> Result<(), Error> {
        call(api, "GET", "repos/sam/r/issues", None, None).map(|_| ())
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
        let result: Result<Option<()>, Error> = run(Some(&wake), &mut api, 1000, |api| {
            let _ = ask(api);
            Err("disk failure".into())
        });
        assert_eq!(result, Err("disk failure".into()));
    }
    #[test]
    fn secondary_limits_and_missing_reset_book_a_retry() {
        for (status, headers, at) in [
            (429, vec![("retry-after", "90")], 1100),
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
    fn contextual_aggregate_rate_errors_defer_but_mixed_errors_fail() {
        let dir = tempfile::tempdir().unwrap();
        let wake = dir.path().join("wake.json");
        let mut api = hub(403, &[("retry-after", "90")]);
        let result: Result<Option<()>, Error> = run(Some(&wake), &mut api, 1000, |api| {
            let first = ask(api).unwrap_err();
            let second = ask(api).unwrap_err();
            Err(Error::aggregate(vec![first.context("#1"), second.context("#2")]).context("sam/r"))
        });
        assert_eq!(result, Ok(None));
    }
    #[test]
    fn delays_start_when_the_response_arrives() {
        for headers in [
            vec![("retry-after", "90")],
            vec![("x-ratelimit-remaining", "0")],
        ] {
            let dir = tempfile::tempdir().unwrap();
            let wake = dir.path().join("wake.json");
            let mut api = hub(403, &headers);
            api.clock = 1400; // earlier work in this pass
            api.latency = 20;
            assert_eq!(run(Some(&wake), &mut api, 1000, ask), Ok(None));
            let booked: Value = serde_json::from_slice(&fs::read(wake).unwrap()).unwrap();
            let delay = if headers[0].0 == "retry-after" {
                90
            } else {
                60
            };
            assert_eq!(booked[""], 1420 + delay);
        }
    }
    #[test]
    fn graphql_limits_ordinary_errors_and_last_point_success_are_distinct() {
        for (body, limited) in [
            (
                json!({"errors":[{"type":"RATE_LIMITED", "message":"budget exhausted"}]}),
                true,
            ),
            (
                json!({"errors":[{"message":"You have exceeded a secondary rate limit"}]}),
                true,
            ),
            (
                json!({"errors":[{"extensions":{"code":"RATE_LIMITED"}, "message":"budget exhausted"}]}),
                true,
            ),
            (
                json!({"errors":[{"type":"NOT_FOUND", "message":"repository rate limit exceeded not found"}]}),
                false,
            ),
            (
                json!({"errors":[{"type":"RATE_LIMITED"}, {"type":"FORBIDDEN"}]}),
                false,
            ),
            (json!({"data":{"ok":true}}), false),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let wake = dir.path().join("wake.json");
            let mut api = hub(
                200,
                &[
                    ("retry-after", "90"),
                    ("x-ratelimit-remaining", "0"),
                    ("x-ratelimit-reset", "2000"),
                ],
            );
            api.body = body.clone();
            let result = run(Some(&wake), &mut api, 1000, |api| {
                crate::query(api, "sam", "query{}", json!({}))
            });
            if limited {
                assert_eq!(result, Ok(None), "{body}");
                assert_eq!(fs::read_to_string(wake).unwrap(), r#"{"":2000}"#);
            } else if body.get("errors").is_some() {
                assert!(result.is_err(), "{body}");
            } else {
                assert_eq!(result, Ok(Some(json!({"ok":true}))));
                assert_eq!(fs::read_to_string(wake).unwrap(), "{}");
            }
        }
    }

    #[test]
    fn absolute_and_relative_deadlines_keep_the_latest_applicable_time() {
        let mut api = hub(
            403,
            &[
                ("retry-after", "90"),
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset", "1500"),
            ],
        );
        api.clock = 1400;
        api.latency = 20;
        assert_eq!(ask(&mut api).unwrap_err().retry_at, Some(1510));
        api.clock = 1400;
        api.headers[2].1 = "1600".into();
        assert_eq!(ask(&mut api).unwrap_err().retry_at, Some(1600));
        // A long earlier delay still applies if subsequent attempts give
        // shorter ones. Built-in retry waits and latency advance the clock.
        struct Changing {
            clock: u64,
            calls: u64,
        }
        impl Api for Changing {
            fn request(
                &mut self,
                _: &str,
                _: &str,
                _: Option<&Value>,
                _: Option<&str>,
            ) -> Result<Response, Failed> {
                self.clock += 20;
                self.calls += 1;
                Ok(Response {
                    status: 429,
                    headers: vec![(
                        "retry-after".into(),
                        if self.calls == 1 { "600" } else { "90" }.into(),
                    )],
                    body: "{}".into(),
                })
            }
            fn pause(&mut self, secs: u64) {
                self.clock += secs;
            }
            fn now(&self) -> u64 {
                self.clock
            }
        }
        assert_eq!(
            ask(&mut Changing {
                clock: 1000,
                calls: 0
            })
            .unwrap_err()
            .retry_at,
            Some(1620)
        );
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
    fn keyed_bookings_keep_each_others_and_the_latest_time() {
        let dir = tempfile::tempdir().unwrap();
        let wake = dir.path().join("wake.json");
        book(&wake, "Sam/R", 2000).unwrap();
        book(&wake, "", 1500).unwrap();
        book(&wake, "sam/r", 1800).unwrap();
        let booked: Value = serde_json::from_slice(&fs::read(&wake).unwrap()).unwrap();
        assert_eq!(booked, json!({"sam/r": 2000, "": 1500}));
        // A pass that ran before its booking came due leaves it be; one that
        // ran once it had used it up.
        ran(&wake, "SAM/r", 1999).unwrap();
        ran(&wake, "", 1500).unwrap();
        ran(&wake, "kim/q", 1500).unwrap();
        let booked: Value = serde_json::from_slice(&fs::read(&wake).unwrap()).unwrap();
        assert_eq!(booked, json!({"sam/r": 2000}));
        ran(&wake, "sam/r", 2000).unwrap();
        assert_eq!(fs::read_to_string(&wake).unwrap(), "{}");
        // What isn't a wake file is no booking to throw away.
        fs::write(&wake, "[]").unwrap();
        assert!(book(&wake, "sam/r", 2000).is_err());
        assert_eq!(fs::read_to_string(&wake).unwrap(), "[]");
        assert!(book(&dir.path().join("absent/wake.json"), "", 2000).is_err());
    }

    #[test]
    fn a_rebooking_replaces_its_own_keys_time_either_way() {
        let dir = tempfile::tempdir().unwrap();
        let wake = dir.path().join("wake.json");
        book(&wake, "", 3000).unwrap();
        book(&wake, "sam/r", 1000).unwrap();
        // Sooner than a later booking, and later than a due one.
        rebook(&wake, "", 2000).unwrap();
        rebook(&wake, "Sam/R", 4000).unwrap();
        let booked: Value = serde_json::from_slice(&fs::read(&wake).unwrap()).unwrap();
        assert_eq!(booked, json!({"": 2000, "sam/r": 4000}));
        // Missing, it's added beside the rest.
        rebook(&wake, "kim/q", 5).unwrap();
        let booked: Value = serde_json::from_slice(&fs::read(&wake).unwrap()).unwrap();
        assert_eq!(booked, json!({"": 2000, "sam/r": 4000, "kim/q": 5}));
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
