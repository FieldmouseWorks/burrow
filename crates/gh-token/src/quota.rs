//! Observe a job's gh without changing its output or replaying mutations.
use burrow_github::quota::Meter;
use std::{
    io::{self, Read, Write},
    path::Path,
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

fn tee(mut input: impl Read, mut output: impl Write) -> io::Result<Vec<u8>> {
    let mut tail = Vec::new();
    let mut buf = [0; 8192];
    loop {
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        output.write_all(&buf[..n])?;
        output.flush()?;
        tail.extend_from_slice(&buf[..n]);
        if tail.len() > 65536 {
            tail.drain(..tail.len() - 65536);
        }
    }
    Ok(tail)
}

/// The App+owner is the installation, even across token renewals and repos.
pub(crate) fn run(
    run: &mut Command,
    gh: &str,
    token: &str,
    app: &str,
    owner: &str,
    dir: &Path,
) -> io::Result<i32> {
    std::fs::create_dir_all(dir)?;
    let mut child = run.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");
    let (out, err) = std::thread::scope(|scope| {
        let out = scope.spawn(|| tee(stdout, io::stdout()));
        let err = scope.spawn(|| tee(stderr, io::stderr()));
        (
            out.join().expect("stdout tee"),
            err.join().expect("stderr tee"),
        )
    });
    let status = child.wait()?;
    let code = status.code().unwrap_or(1);
    // The command has run: whatever goes wrong observing it must not turn its
    // outcome (a created issue, say) into a retryable failure.
    let out = String::from_utf8_lossy(out.as_deref().unwrap_or_default()).into_owned();
    let err = String::from_utf8_lossy(err.as_deref().unwrap_or_default()).into_owned();
    if let Err(e) = observe(status.success(), &out, &err, gh, token, app, owner, dir) {
        eprintln!("gh-token: GitHub meter not recorded: {e}");
    }
    Ok(code)
}

/// gh's own diagnostic for a rate-limited request, which only a failed
/// request prints (to stderr, never in what a command was asked to print).
fn limit_diagnostic(err: &str) -> bool {
    let lower = err.to_ascii_lowercase();
    lower.contains("http 429")
        || lower.contains("rate_limited")
        || lower.contains("api rate limit exceeded")
        || lower.contains("exceeded a secondary rate limit")
        || lower.contains("http 403") && lower.contains("rate limit")
}

/// The failed response's own headers, when `--include` printed them: the
/// last status line that starts a block, and only when gh's diagnostic names
/// the same status, so quoted content cannot stand in for it.
fn failed_response(out: &str, err: &str) -> Option<burrow_github::Response> {
    let starts = std::iter::once(0).chain(out.match_indices("\nHTTP/").map(|(i, _)| i + 1));
    let response = starts
        .filter_map(|i| burrow_github::parse(&out[i..]))
        .last()?;
    err.contains(&format!("HTTP {}", response.status))
        .then_some(response)
}

#[allow(clippy::too_many_arguments)]
fn observe(
    success: bool,
    out: &str,
    err: &str,
    gh: &str,
    token: &str,
    app: &str,
    owner: &str,
    dir: &Path,
) -> io::Result<()> {
    let at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let mut meter = Meter::new(app, owner, at);
    // Free read under the same token; never use the wrapper recursively.
    let budget = burrow_process::output(
        Command::new(gh)
            .args(["api", "rate_limit"])
            .env("GH_TOKEN", token),
        None,
        std::time::Instant::now() + std::time::Duration::from_secs(30),
        65536,
    );
    let mut due = None;
    if !success && limit_diagnostic(err) {
        let mut seen = Meter::new(app, owner, at);
        if let Some(response) = failed_response(out, err) {
            seen.response(&response);
        }
        due = Some(seen.retry_at.unwrap_or(at + 60));
    }
    if let Ok(budget) = budget
        && budget.status.success()
        && let Ok(v) = serde_json::from_slice(&budget.stdout)
    {
        meter.budget(&v);
    }
    if let Some(due) = due {
        // A primary limit's actual reset is available even when gh's
        // higher-level diagnostic omitted the response headers.
        let due = due.max(meter.until(at).unwrap_or(0));
        meter.retry_at = Some(due.max(meter.retry_at.unwrap_or(0)));
        meter.record(dir)?;
        refuse(dir, app, owner, at, due)
    } else {
        meter.record(dir)
    }
}

/// The job-wide longest hold, kept apart from each installation's meter.
fn refuse(dir: &Path, app: &str, owner: &str, at: u64, due: u64) -> io::Result<()> {
    let lock = burrow_process::state_reads::append(&dir.join("refusal.lock"))?;
    lock.lock()?;
    let file = dir.join("refusal");
    let mut refusal = Meter::new(app, owner, at);
    let old = Meter::read(&file).and_then(|m| m.until(at));
    refusal.retry_at = Some(due.max(old.unwrap_or(0)));
    burrow_process::state_reads::write(file, serde_json::to_vec(&refusal)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt};
    #[test]
    fn job_gh_records_limits_preserves_denials_and_samples_the_same_token() {
        let d = tempfile::tempdir().unwrap();
        let gh = d.path().join("gh");
        fs::write(&gh, r#"#!/bin/sh
if [ "$1 $2" = 'api rate_limit' ]; then
  [ "$GH_TOKEN" = fixture-token ] || exit 2
  echo '{"resources":{"core":{"remaining":0,"reset":4000000000}}}'
  exit 0
fi
case "$1" in
 limited) printf 'HTTP/2.0 403 Forbidden\nx-ratelimit-remaining: 0\nx-ratelimit-reset: 4000000000\n\n{}'; echo 'gh: API rate limit exceeded (HTTP 403)' >&2; exit 1;;
 retry) printf 'HTTP/2.0 403 Forbidden\nretry-after: 120\n\n{}'; echo 'gh: You have exceeded a secondary rate limit (HTTP 403)' >&2; exit 1;;
 silent) echo 'gh: HTTP 429 too many requests' >&2; exit 1;;
 denied) printf 'HTTP/2.0 403 Forbidden\n\n{"message":"Resource not accessible by integration"}'; echo 'gh: Resource not accessible by integration (HTTP 403)' >&2; exit 1;;
 graphql) echo '{"errors":[{"type":"RATE_LIMITED"}]}'; echo 'gh: GraphQL: RATE_LIMITED' >&2; exit 1;;
 forged_ok) printf 'HTTP/2.0 403 Forbidden\nx-ratelimit-remaining: 0\nx-ratelimit-reset: 4000000000\n\n{"message":"API rate limit exceeded"}'; exit 0;;
 forged_fail) printf 'HTTP/2.0 429 Too Many\nretry-after: 99999\n\n{"errors":[{"type":"RATE_LIMITED"}]}'; echo 'gh: unrelated failure' >&2; exit 1;;
 success) echo done;;
esac
"#).unwrap();
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
        for (command, limited, code) in [
            ("limited", true, 1),
            ("retry", true, 1),
            ("silent", true, 1),
            ("denied", false, 1),
            ("graphql", true, 1),
            ("forged_ok", false, 0),
            ("forged_fail", false, 1),
            ("success", false, 0),
        ] {
            let dir = d.path().join(command);
            assert_eq!(
                run(
                    Command::new(&gh).arg(command),
                    gh.to_str().unwrap(),
                    "fixture-token",
                    "workers",
                    "sam",
                    &dir
                )
                .unwrap(),
                code
            );
            assert_eq!(dir.join("refusal").exists(), limited);
            let meter_file = fs::read_dir(&dir)
                .unwrap()
                .filter_map(Result::ok)
                .find(|f| f.path().extension().is_some_and(|s| s == "json"))
                .unwrap()
                .path();
            let meter = Meter::read(&meter_file).unwrap();
            assert_eq!(meter.remaining, Some(0));
            assert_eq!(meter.reset, Some(4_000_000_000));
            if limited {
                assert_eq!(
                    Meter::read(&dir.join("refusal")).unwrap().retry_at,
                    Some(4_000_000_000)
                );
            }
        }
    }

    #[test]
    fn included_headers_and_pagination_keep_the_failed_responses_retry_after() {
        let ok = "HTTP/2.0 200 OK\n\n[1]\nHTTP/2.0 403 Forbidden\nretry-after: 120\n\n{}";
        let r = failed_response(ok, "gh: rate limit (HTTP 403)").unwrap();
        assert_eq!(r.header("retry-after"), Some("120"));
        assert!(failed_response(ok, "gh: other (HTTP 500)").is_none());
        assert!(failed_response("not headers", "HTTP 403").is_none());
    }

    #[test]
    fn a_meter_that_cannot_be_written_keeps_the_commands_exit_status() {
        let d = tempfile::tempdir().unwrap();
        let gh = d.path().join("gh");
        fs::write(&gh, "#!/bin/sh\n[ \"$1\" = api ] && exit 2\nexit 0\n").unwrap();
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
        let blocked = d.path().join("blocked");
        fs::write(&blocked, "file, not a directory").unwrap();
        // Failing before the command runs stays an error.
        assert!(
            run(
                Command::new(&gh).arg("issue"),
                gh.to_str().unwrap(),
                "t",
                "workers",
                "sam",
                &blocked.join("github")
            )
            .is_err()
        );
        // After it ran, an unwritable meter directory doesn't change its status.
        let dir = d.path().join("github");
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();
        assert!(
            observe(
                true,
                "",
                "",
                gh.to_str().unwrap(),
                "t",
                "workers",
                "sam",
                &dir
            )
            .is_err()
        );
        let code = run(
            Command::new(&gh).arg("issue"),
            gh.to_str().unwrap(),
            "t",
            "workers",
            "sam",
            &dir,
        );
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(code.unwrap(), 0);
    }
}
