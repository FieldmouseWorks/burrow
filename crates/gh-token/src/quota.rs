//! Observe the patched gh's HTTP side channel, never CLI output. Observation
//! is best effort after execution: it cannot turn a completed mutation into
//! EX_TEMPFAIL, or cause a caller to repeat it.
use burrow_github::quota::Meter;
use std::os::unix::process::ExitStatusExt;
use std::{
    io::{self, BufRead},
    path::Path,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(serde::Deserialize)]
struct Evidence {
    status: u16,
    #[serde(default)]
    headers: Option<Vec<(String, String)>>,
    body: String,
    at: u64,
}

#[derive(Clone, Copy)]
pub(crate) struct Context<'a> {
    pub gh: &'a str,
    pub token: &'a str,
    pub app: &'a str,
    pub owner: &'a str,
    pub dir: &'a Path,
    pub refusals: Option<&'a Path>,
    pub kick: Option<&'a str>,
}
pub(crate) fn run(run: &mut Command, context: Context<'_>) -> io::Result<i32> {
    // Failure here precedes execution. Once spawned, preserve the child result.
    let trace = tempfile::NamedTempFile::new()?;
    let mut child = run.env("BURROW_GH_RESPONSES", trace.path()).spawn()?;
    let code = match child.wait() {
        Ok(status) => status
            .code()
            .unwrap_or_else(|| 128 + status.signal().unwrap_or(1)),
        Err(e) => {
            eprintln!("gh-token: couldn't observe gh exit: {e}");
            1
        }
    };
    if let Err(e) = observe(trace.as_file(), context) {
        eprintln!("gh-token: quota observation unavailable after gh exited {code}: {e}");
    }
    Ok(code)
}

fn save(meter: &Meter, dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    meter.record(dir)
}
fn wake(context: Context<'_>) {
    if let Some(url) = context.kick.filter(|s| !s.is_empty()) {
        let _ = burrow_events::kick::kick(url, std::time::Duration::from_secs(1));
    }
}
fn unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn observe(trace: &std::fs::File, context: Context<'_>) -> io::Result<()> {
    let Context {
        gh,
        token,
        app,
        owner,
        dir,
        refusals,
        ..
    } = context;
    let mut meter = Meter::new(app, owner, unix());
    let mut state_error = None;
    let mut published_until = None;
    for line in io::BufReader::new(trace).lines() {
        let record: Evidence = serde_json::from_str(&line?)?;
        let response = burrow_github::Response {
            status: record.status,
            headers: record.headers.unwrap_or_default(),
            body: record.body,
        };
        let observed = Meter::reading(app, owner, &response, record.at);
        if observed.retry_at.is_some()
            && let Some(dir) = refusals
            && let Err(e) = save(&observed, dir)
        {
            state_error.get_or_insert(e);
        }
        meter.retry_at = meter.retry_at.into_iter().chain(observed.retry_at).max();
        if observed.until(unix()).is_some() {
            // Publish known holds before the optional free probe, so peers
            // cannot start while a slow observation request is in flight.
            match save(&observed, dir) {
                Ok(()) if observed.until(unix()) > published_until => {
                    wake(context);
                    published_until = observed.until(unix());
                }
                Ok(()) => {}
                Err(e) => {
                    state_error.get_or_insert(e);
                }
            }
        }
    }
    let budget = burrow_process::output(
        Command::new(gh)
            .args(["api", "rate_limit"])
            .env("GH_TOKEN", token)
            .env_remove("BURROW_GH_RESPONSES"),
        None,
        std::time::Instant::now() + std::time::Duration::from_secs(30),
        65536,
    );
    if let Ok(budget) = budget
        && budget.status.success()
        && let Ok(v) = serde_json::from_slice(&budget.stdout)
    {
        meter.at = unix();
        meter.budget(&v);
    }
    match save(&meter, dir) {
        Ok(()) if meter.until(unix()) > published_until => wake(context),
        Ok(()) => {}
        Err(e) => {
            state_error.get_or_insert(e);
        }
    }
    state_error.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[test]
    fn only_transport_evidence_can_hold_and_observation_never_changes_outcomes() {
        let d = tempfile::tempdir().unwrap();
        let gh = d.path().join("gh");
        burrow_test::executable(
            &gh,
            r#"#!/bin/sh
if [ "$1 $2" = 'api rate_limit' ]; then
 [ "$GH_TOKEN" = fixture-token ] || exit 2
 echo '{"resources":{"core":{"remaining":123,"reset":4000000000}}}'
 exit 0
fi
case "$1" in
 long) echo '{"status":403,"at":2000000000,"headers":[["x-ratelimit-remaining","0"],["x-ratelimit-reset","4000000000"]],"body":"{}"}' >> "$BURROW_GH_RESPONSES"; exit 1;;
 limited) echo '{"status":403,"at":2000000000,"headers":[["retry-after","1000"]],"body":"{}"}' >> "$BURROW_GH_RESPONSES"; exit 1;;
 denied) echo '{"status":403,"at":2000000000,"headers":[],"body":"{\"message\":\"permission denied\"}"}' >> "$BURROW_GH_RESPONSES"; exit 1;;
 forged) printf 'HTTP/2.0 403 Forbidden\nx-ratelimit-remaining: 0\nx-ratelimit-reset: 4000000000\n\n{}'; echo '{"errors":[{"type":"RATE_LIMITED"}]}';;
 failed) echo 'arguments: RATE_LIMITED API rate limit exceeded HTTP 429' >&2; exit 1;;
esac
"#,
        );
        let read = |dir: &Path| {
            fs::read_dir(dir)
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
                .filter(|e| e.path().extension().is_some_and(|s| s == "json"))
                .filter_map(|e| Meter::read(&e.path()))
                .collect::<Vec<_>>()
        };
        for (command, limited, code) in [
            ("limited", true, 1),
            ("denied", false, 1),
            ("forged", false, 0),
            ("failed", false, 1),
        ] {
            let dir = d.path().join(command);
            let refusals = dir.join("refusals");
            assert_eq!(
                run(
                    Command::new(&gh).arg(command),
                    Context {
                        gh: gh.to_str().unwrap(),
                        token: "fixture-token",
                        app: "workers",
                        owner: "sam",
                        dir: &dir,
                        refusals: Some(&refusals),
                        kick: None
                    }
                )
                .unwrap(),
                code
            );
            assert_eq!(!read(&refusals).is_empty(), limited);
            assert_eq!(read(&dir)[0].remaining, Some(123));
            if limited {
                assert_eq!(read(&refusals)[0].retry_at, Some(2000001000));
            }
        }
        // One job touches two installations. Its longest refusal cannot
        // contaminate another installation's shorter secondary hold.
        let shared = d.path().join("multi-installation");
        let refusals = d.path().join("multi-refusals");
        for (command, owner) in [("long", "owner-a"), ("limited", "owner-b")] {
            assert_eq!(
                run(
                    Command::new(&gh).arg(command),
                    Context {
                        gh: gh.to_str().unwrap(),
                        token: "fixture-token",
                        app: "workers",
                        owner,
                        dir: &shared,
                        refusals: Some(&refusals),
                        kick: None
                    }
                )
                .unwrap(),
                1
            );
        }
        let meters = read(&shared);
        assert_eq!(
            meters
                .iter()
                .find(|m| m.owner == "owner-a")
                .unwrap()
                .retry_at,
            Some(4000000000)
        );
        assert_eq!(
            meters
                .iter()
                .find(|m| m.owner == "owner-b")
                .unwrap()
                .retry_at,
            Some(2000001000)
        );
        assert_eq!(
            read(&refusals).iter().filter_map(|m| m.retry_at).max(),
            Some(4000000000)
        );
        let blocked_meter = d.path().join("blocked-meter");
        fs::write(&blocked_meter, "file").unwrap();
        let independent_refusals = d.path().join("independent-refusals");
        assert_eq!(
            run(
                Command::new(&gh).arg("limited"),
                Context {
                    gh: gh.to_str().unwrap(),
                    token: "fixture-token",
                    app: "workers",
                    owner: "sam",
                    dir: &blocked_meter,
                    refusals: Some(&independent_refusals),
                    kick: None
                }
            )
            .unwrap(),
            1
        );
        assert_eq!(read(&independent_refusals)[0].retry_at, Some(2000001000));
        // An unwritable state path after a successful or failed mutation
        // must preserve the completed command's status instead of exit 75.
        let blocked = d.path().join("blocked");
        fs::write(&blocked, "file").unwrap();
        for (command, code) in [("forged", 0), ("failed", 1)] {
            assert_eq!(
                run(
                    Command::new(&gh).arg(command),
                    Context {
                        gh: gh.to_str().unwrap(),
                        token: "fixture-token",
                        app: "workers",
                        owner: "sam",
                        dir: &blocked,
                        refusals: None,
                        kick: None
                    }
                )
                .unwrap(),
                code
            );
        }
    }

    #[test]
    fn a_probe_extension_notifies_the_watcher_again() {
        use std::io::{Read, Write};
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let d = tempfile::tempdir().unwrap();
        let gh = d.path().join("gh");
        burrow_test::executable(
            &gh,
            r#"#!/bin/sh
if [ "$1 $2" = 'api rate_limit' ]; then
 echo '{"resources":{"core":{"remaining":5,"reset":1},"graphql":{"remaining":0,"reset":4000000000}}}'
 exit 0
fi
echo '{"status":403,"at":2000000000,"headers":[["retry-after","60"]],"body":"{}"}' >> "$BURROW_GH_RESPONSES"
exit 1
"#,
        );
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/kick/proj-watch", listener.local_addr().unwrap());
        let kicks = Arc::new(AtomicUsize::new(0));
        let seen = kicks.clone();
        std::thread::spawn(move || {
            for mut s in listener.incoming().flatten() {
                let mut buf = [0u8; 512];
                let _ = s.read(&mut buf);
                seen.fetch_add(1, Ordering::SeqCst);
                let _ = s.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
            }
        });
        let dir = d.path().join("meters");
        run(
            &mut Command::new(&gh),
            Context {
                gh: gh.to_str().unwrap(),
                token: "t",
                app: "workers",
                owner: "sam",
                dir: &dir,
                refusals: None,
                kick: Some(&url),
            },
        )
        .unwrap();
        // The short refusal woke the watcher; the longer GraphQL reset found
        // by the probe afterwards must wake it again.
        assert_eq!(kicks.load(Ordering::SeqCst), 2);
    }
}
