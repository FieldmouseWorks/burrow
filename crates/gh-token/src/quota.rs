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

pub(crate) fn run(
    run: &mut Command,
    gh: &str,
    token: &str,
    app: &str,
    owner: &str,
    dir: &Path,
    refusals: Option<&Path>,
) -> io::Result<i32> {
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
    if let Err(e) = observe(trace.as_file(), gh, token, app, owner, dir, refusals) {
        eprintln!("gh-token: quota observation unavailable after gh exited {code}: {e}");
    }
    Ok(code)
}

fn observe(
    trace: &std::fs::File,
    gh: &str,
    token: &str,
    app: &str,
    owner: &str,
    dir: &Path,
    refusals: Option<&Path>,
) -> io::Result<()> {
    let at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let mut meter = Meter::new(app, owner, at);
    for line in io::BufReader::new(trace).lines() {
        let record: Evidence = serde_json::from_str(&line?)?;
        let mut refusal = Meter::new(app, owner, record.at);
        refusal.response(&burrow_github::Response {
            status: record.status,
            headers: record.headers.unwrap_or_default(),
            body: record.body,
        });
        if refusal.retry_at.is_some() {
            if let Some(dir) = refusals {
                std::fs::create_dir_all(dir)?;
                refusal.record(dir)?;
            }
            meter.retry_at = meter.retry_at.into_iter().chain(refusal.retry_at).max();
        }
    }
    // This read costs no budget, uses precisely the same token, and cannot
    // recursively record evidence or alter the command's outcome.
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
        meter.at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        meter.budget(&v);
    }
    std::fs::create_dir_all(dir)?;
    meter.record(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt};
    #[test]
    fn only_transport_evidence_can_hold_and_observation_never_changes_outcomes() {
        let d = tempfile::tempdir().unwrap();
        let gh = d.path().join("gh");
        fs::write(&gh,r#"#!/bin/sh
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
"#).unwrap();
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
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
                    gh.to_str().unwrap(),
                    "fixture-token",
                    "workers",
                    "sam",
                    &dir,
                    Some(&refusals)
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
                    gh.to_str().unwrap(),
                    "fixture-token",
                    "workers",
                    owner,
                    &shared,
                    Some(&refusals)
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
                gh.to_str().unwrap(),
                "fixture-token",
                "workers",
                "sam",
                &blocked_meter,
                Some(&independent_refusals)
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
                    gh.to_str().unwrap(),
                    "fixture-token",
                    "workers",
                    "sam",
                    &blocked,
                    None
                )
                .unwrap(),
                code
            );
        }
    }
}
