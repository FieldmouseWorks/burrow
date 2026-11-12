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
    let mut refusal = Meter::new(app, owner, at);
    let out = String::from_utf8_lossy(out.as_ref().map(Vec::as_slice).unwrap_or_default());
    let err = String::from_utf8_lossy(err.as_ref().map(Vec::as_slice).unwrap_or_default());
    if let Some(response) = burrow_github::parse(&out) {
        refusal.response(&response);
    } else if let Ok(body) = serde_json::from_str::<serde_json::Value>(&out)
        && body.get("errors").is_some()
    {
        refusal.response(&burrow_github::Response {
            status: 200,
            headers: vec![],
            body: out.to_string(),
        });
    } else if !status.success() {
        let lower = err.to_ascii_lowercase();
        if lower.contains("http 429")
            || lower.contains("rate_limited")
            || lower.contains("api rate limit exceeded")
            || lower.contains("exceeded a secondary rate limit")
            || lower.contains("http 403") && lower.contains("rate limit")
        {
            refusal.retry_at = Some(at + 60);
        }
    }
    if let Ok(budget) = budget
        && budget.status.success()
        && let Ok(v) = serde_json::from_slice(&budget.stdout)
    {
        meter.budget(&v);
    }
    if let Some(due) = refusal.retry_at {
        // A primary limit's actual reset is available even when gh's
        // higher-level diagnostic omitted the response headers.
        let due = due.max(meter.until(at).unwrap_or(0)).max(
            Meter::read(&dir.join("refusal"))
                .and_then(|m| m.retry_at)
                .unwrap_or(0),
        );
        meter.retry_at = Some(due);
        refusal.retry_at = Some(due);
        burrow_process::state_reads::write(dir.join("refusal"), serde_json::to_vec(&refusal)?)?;
    }
    meter.record(dir)?;
    Ok(status.code().unwrap_or(1))
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
 limited) printf 'HTTP/2.0 403 Forbidden\nx-ratelimit-remaining: 0\nx-ratelimit-reset: 4000000000\n\n{}'; exit 1;;
 denied) printf 'HTTP/2.0 403 Forbidden\n\n{"message":"Resource not accessible by integration"}'; exit 1;;
 graphql) echo '{"errors":[{"type":"RATE_LIMITED"}]}'; exit 1;;
 success) echo done;;
esac
"#).unwrap();
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
        for (command, limited, code) in [
            ("limited", true, 1),
            ("denied", false, 1),
            ("graphql", true, 1),
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
}
