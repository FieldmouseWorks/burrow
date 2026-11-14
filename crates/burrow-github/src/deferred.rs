//! Durable rate-limit deferral: a pass GitHub held off, booked again in its
//! burrow-events wake file for when the limit lifts. A pass runs under its
//! installation's meter (quota.rs): held already, it's booked without a
//! GitHub call; held partway, the meter is recorded and the pass booked.
use crate::quota::{Hold, Metered, Meters};
use crate::{Api, Error};
use serde_json::{Map, Value, json};
use std::{fs, path::Path};

/// The issue-door services configure their own wake file. Local commands do
/// not, so a transient failure cannot silently become success without a retry.
/// A door that acts as an App is given its installation's meter too
/// (BURROW_DOOR_METER, [`DoorMeter`]), and runs under it.
pub fn configured<T>(
    pass: impl FnOnce(&mut dyn Api) -> Result<T, Error>,
) -> Result<Option<T>, String> {
    let wake = std::env::var_os("BURROW_DOOR_WAKE").map(std::path::PathBuf::from);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();
    let kick = std::env::var("BURROW_DOOR_WATCHDOG_KICK").ok();
    let door = DoorMeter::parse(&std::env::var("BURROW_DOOR_METER").unwrap_or_default())?;
    let mut meters = door
        .as_ref()
        .map(|d| (Meters::new(Some(d.dir.clone()), &d.app), d.owner.as_str()));
    run_kicking(
        wake.as_deref(),
        kick.as_deref(),
        &mut crate::Gh::default(),
        now,
        meters.as_mut().map(|(m, owner)| (m, *owner)),
        pass,
    )
    .map_err(Into::into)
}

/// BURROW_DOOR_METER, as modules/github.nix's doorMeter writes it: the
/// meters' dir, the App the door acts as and the owner of its repo, as
/// JSON; empty for a door that acts as no App.
#[derive(Debug, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DoorMeter {
    pub dir: std::path::PathBuf,
    pub app: String,
    pub owner: String,
}

impl DoorMeter {
    pub fn parse(text: &str) -> Result<Option<Self>, String> {
        if text.trim().is_empty() {
            return Ok(None);
        }
        serde_json::from_str(text)
            .map(Some)
            .map_err(|e| format!("BURROW_DOOR_METER: {e}"))
    }
}

/// Run a pass, preserving a deferred full pass in burrow-events' wake file.
/// Without a wake file, errors retain their usual meaning.
pub fn run<T>(
    wake: Option<&Path>,
    api: &mut dyn Api,
    now: u64,
    pass: impl FnOnce(&mut dyn Api) -> Result<T, Error>,
) -> Result<Option<T>, Error> {
    run_kicking(wake, None, api, now, None, pass)
}

/// The hold on an installation, as the error a pass held off by it gives:
/// for a caller with no wake file to book it in.
pub fn held(hold: &Hold) -> Error {
    Error {
        why: format!(
            "GitHub holds {}'s installation on {} off until {}",
            hold.app, hold.owner, hold.until
        ),
        unanswered: true,
        retry_at: Some(hold.until),
    }
}

/// PASS under METER, the meters of an App and the owner of the
/// installation the pass is bound to (its calls that name no other): on a
/// client that feeds each answer to the meter of the installation it was
/// about, and refuses, without asking GitHub, a call to one its meter
/// holds already ([`Metered::refusing`]). A pass held off is booked for the
/// latest hold among the installations it met one on, which the meter can
/// know to outlast what the answer said; no other installation is held for
/// it, the pass's own included.
pub fn metered<T>(
    meter: Option<(&mut Meters, &str)>,
    api: &mut dyn Api,
    now: u64,
    pass: impl FnOnce(&mut dyn Api) -> Result<T, Error>,
) -> Result<T, Error> {
    let Some((meters, owner)) = meter else {
        return pass(api);
    };
    let mut client = Metered::new(api, meters, Some(owner.into())).refusing();
    let done = pass(&mut client);
    let met = client.met().clone();
    meters.record();
    done.map_err(|e| match e.retry_at {
        Some(retry_at) => Error {
            retry_at: met
                .iter()
                .filter_map(|owner| meters.held(owner, now))
                .map(|hold| hold.until)
                .chain([retry_at])
                .max(),
            ..e
        },
        None => e,
    })
}

/// [`run`], kicking the watchdog at KICK (a loopback http URL) when the
/// door's deferral starts or ends, under METER ([`metered`]) when it's
/// given.
pub fn run_kicking<T>(
    wake: Option<&Path>,
    kick: Option<&str>,
    api: &mut dyn Api,
    now: u64,
    meter: Option<(&mut Meters, &str)>,
    pass: impl FnOnce(&mut dyn Api) -> Result<T, Error>,
) -> Result<Option<T>, Error> {
    let Some(wake) = wake else {
        return metered(meter, api, now, pass).map(Some);
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
    match metered(meter, api, now, pass) {
        Ok(done) => {
            save(wake, None)?;
            watch(wake, kick, now, None)?;
            Ok(Some(done))
        }
        Err(e) => {
            match e.retry_at {
                Some(at) => {
                    save(wake, Some(at))?;
                    watch(wake, kick, now, Some(at))?;
                    eprintln!("GitHub rate limited this pass; pending until {at}: {e}");
                    Ok(None)
                }
                None => {
                    // The due booking has been consumed. Genuine failures use
                    // burrow-events' failure backoff, not a stale past wake.
                    if booked.is_some() {
                        save(wake, None)?;
                    }
                    // Not a rate limit now: an earlier deferral is over.
                    if let Err(w) = watch(wake, kick, now, None) {
                        eprintln!("{w}");
                    }
                    Err(e)
                }
            }
        }
    }
}

/// How long a door may sit deferred before it is a stall for the watchdog.
/// A limit that lifts sooner is an ordinary wait; a door that answers in
/// seconds and has said nothing for longer than this is late.
pub const DEFERRED_GRACE: u64 = 300;

/// The watch file beside WAKE, for the stall watchdog's source of the door.
pub fn watch_file(wake: &Path) -> std::path::PathBuf {
    wake.with_extension("watch.json")
}

/// Record in the watch file whether the door's pass is deferred (AT: until
/// when) or ran. A deferral is an item whose `since` is kept across passes
/// and whose deadline is DEFERRED_GRACE after it, so a door held off past
/// its deadline is a stall, not silence. Passes that defer again keep the
/// first `since`.
fn watch(wake: &Path, kick: Option<&str>, now: u64, deferred: Option<u64>) -> Result<(), String> {
    let path = watch_file(wake);
    let since = fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v["items"]["deferred"]["since"].as_u64());
    // The watchdog takes no GitHub events: it learns of a new deferral (to
    // schedule its deadline) or a cleared one only when kicked.
    let changed = since.is_some() != deferred.is_some();
    let items = match deferred {
        None => json!({}),
        Some(at) => {
            let since = since.unwrap_or(now);
            json!({"deferred": {
                "what": "the door's pass, held off by GitHub's rate limit",
                "phase": format!("deferred until {at}"),
                "since": since,
                "due": since + DEFERRED_GRACE,
            }})
        }
    };
    write(&path, &json!({"at": now, "items": items}))?;
    if let (true, Some(url)) = (changed, kick) {
        kick_watchdog(url);
    }
    Ok(())
}

/// POST to the watchdog's kick URL (BURROW_DOOR_WATCHDOG_KICK, a loopback
/// http URL burrow-events serves), set when the machine has a watchdog. A failed
/// kick is logged: the watchdog's own catch-up still reads the file.
fn kick_watchdog(url: &str) {
    use std::io::{Read, Write};
    let sent = || -> std::io::Result<()> {
        let rest = url
            .strip_prefix("http://")
            .ok_or(std::io::ErrorKind::InvalidInput)?;
        let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
        let timeout = std::time::Duration::from_secs(10);
        let addr = std::net::ToSocketAddrs::to_socket_addrs(host)?
            .next()
            .ok_or(std::io::ErrorKind::InvalidInput)?;
        let mut tcp = std::net::TcpStream::connect_timeout(&addr, timeout)?;
        tcp.set_read_timeout(Some(timeout))?;
        tcp.write_all(
            format!("POST /{path} HTTP/1.0\r\nHost: {host}\r\nContent-Length: 0\r\n\r\n")
                .as_bytes(),
        )?;
        let mut reply = String::new();
        tcp.read_to_string(&mut reply)?;
        if reply
            .split_whitespace()
            .nth(1)
            .is_some_and(|s| s.starts_with('2'))
        {
            Ok(())
        } else {
            Err(std::io::Error::other(
                reply.lines().next().unwrap_or("").to_owned(),
            ))
        }
    };
    if let Err(e) = sent() {
        eprintln!("couldn't kick the watchdog at {url}: {e}");
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
        use std::io::Write;
        let temp = path.with_extension("new");
        let mut file = fs::File::create(&temp)?;
        file.write_all(booked.to_string().as_bytes())?;
        // The scheduler is a different account, even if a caller has a
        // private umask. This file contains only the public retry deadline.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o644))?;
        }
        file.sync_all()?;
        fs::rename(&temp, path)?;
        fs::File::open(path.parent().unwrap_or(Path::new(".")))?.sync_all()
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
    fn a_deferral_past_its_grace_is_a_watched_stall_and_a_pass_clears_it() {
        let dir = tempfile::tempdir().unwrap();
        let wake = dir.path().join("wake.json");
        let watched =
            || -> Value { serde_json::from_slice(&fs::read(watch_file(&wake)).unwrap()).unwrap() };
        let mut api = hub(
            403,
            &[
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset", "5000"),
            ],
        );
        assert_eq!(run(Some(&wake), &mut api, 1000, ask), Ok(None));
        let item = &watched()["items"]["deferred"];
        assert_eq!(
            (item["since"].as_u64(), item["due"].as_u64()),
            (Some(1000), Some(1000 + DEFERRED_GRACE))
        );
        // The retry at the lift is limited again: the wait keeps its start.
        assert_eq!(run(Some(&wake), &mut api, 5000, ask), Ok(None));
        assert_eq!(watched()["items"]["deferred"]["since"], 1000);
        api.status = 200;
        assert_eq!(run(Some(&wake), &mut api, 9000, ask), Ok(Some(())));
        assert_eq!(watched()["items"], json!({}));
    }
    #[test]
    fn a_retry_failing_otherwise_clears_the_deferral_and_keeps_its_error() {
        let dir = tempfile::tempdir().unwrap();
        let wake = dir.path().join("wake.json");
        let mut limited = hub(
            403,
            &[
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset", "2000"),
            ],
        );
        assert_eq!(run(Some(&wake), &mut limited, 1000, ask), Ok(None));
        let failed: Result<Option<()>, Error> = run(Some(&wake), &mut limited, 2000, |_| {
            Err("disk failure".into())
        });
        assert_eq!(failed, Err("disk failure".into()));
        let watched: Value = serde_json::from_slice(&fs::read(watch_file(&wake)).unwrap()).unwrap();
        assert_eq!(watched["items"], json!({}));
        assert_eq!(run(Some(&wake), &mut limited, 3000, ask), Ok(None));
        let watched: Value = serde_json::from_slice(&fs::read(watch_file(&wake)).unwrap()).unwrap();
        assert_eq!(watched["items"]["deferred"]["since"], 3000);
    }
    #[test]
    fn a_new_and_a_cleared_deferral_kick_the_watchdog_once_each() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/kick/watchdog", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let mut paths = Vec::new();
            for _ in 0..2 {
                let (mut c, _) = listener.accept().unwrap();
                let mut buf = [0u8; 512];
                let n = c.read(&mut buf).unwrap();
                paths.push(
                    String::from_utf8_lossy(&buf[..n])
                        .lines()
                        .next()
                        .unwrap()
                        .to_owned(),
                );
                c.write_all(b"HTTP/1.0 200 OK\r\n\r\n").unwrap();
            }
            paths
        });
        let dir = tempfile::tempdir().unwrap();
        let wake = dir.path().join("wake.json");
        let mut api = hub(
            403,
            &[
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset", "2000"),
            ],
        );
        assert_eq!(
            run_kicking(Some(&wake), Some(&url), &mut api, 1000, None, ask),
            Ok(None)
        );
        assert_eq!(
            run_kicking(Some(&wake), Some(&url), &mut api, 2000, None, ask),
            Ok(None)
        );
        api.status = 200;
        assert_eq!(
            run_kicking(Some(&wake), Some(&url), &mut api, 3000, None, ask),
            Ok(Some(()))
        );
        assert_eq!(
            server.join().unwrap(),
            vec!["POST /kick/watchdog HTTP/1.0"; 2]
        );
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
