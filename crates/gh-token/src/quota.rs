//! Observe a job's gh without changing its output or replaying mutations.
use burrow_github::quota::Meter;
use std::{
    io::{self, BufRead, Read, Write},
    path::Path,
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

fn tee(mut input: impl Read, mut output: impl Write) -> io::Result<()> {
    io::copy(&mut input, &mut output).map(drop)
}

/// gh's own transport log (`GH_DEBUG=api`) is the only request metadata a
/// wrapper can trust: it is written by the HTTP client for each request, never
/// by what a command prints. Its blocks run from `* Request at` to
/// `* Request took`; the response's status and headers are the first `< `
/// lines after the request's `> ` lines, so a body, however it is quoted,
/// cannot stand in for them. Transport lines are dropped from what the job
/// sees unless it asked for them; everything else passes through unchanged.
/// Returns the last response, which is the one that failed when a command did.
fn transport(
    input: impl Read,
    mut output: impl Write,
    show: bool,
) -> io::Result<Option<burrow_github::Response>> {
    #[derive(PartialEq)]
    enum At {
        Outside,
        Request,
        Headers,
        Body,
    }
    let mut input = io::BufReader::new(input);
    let (mut at, mut last) = (At::Outside, None);
    let mut line = Vec::new();
    let mut response: Option<burrow_github::Response> = None;
    loop {
        line.clear();
        if input.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        let text = String::from_utf8_lossy(&line);
        let text = text.trim_end_matches(['\n', '\r']);
        let inside = at != At::Outside;
        if text.starts_with("* Request at ") {
            at = At::Request;
            response = None;
        } else if inside && text.starts_with("* Request took ") {
            last = response.take().or(last);
            at = At::Outside;
        } else if at == At::Request && text.starts_with("> ") {
        } else if at == At::Request
            && let Some(status) = text.strip_prefix("< HTTP/")
        {
            let code = status
                .split_whitespace()
                .nth(1)
                .and_then(|c| c.parse().ok());
            response = code.map(|status| burrow_github::Response {
                status,
                headers: Vec::new(),
                body: String::new(),
            });
            at = At::Headers;
        } else if at == At::Headers
            && let Some(header) = text.strip_prefix("< ")
        {
            if let (Some(r), Some((name, value))) = (&mut response, header.split_once(':')) {
                r.headers
                    .push((name.trim().to_owned(), value.trim().to_owned()));
            }
        } else if at == At::Headers && text.is_empty() {
            at = At::Body;
        } else if at == At::Body {
            if let Some(r) = &mut response
                && r.body.len() < 65536
            {
                r.body.push_str(text);
                r.body.push('\n');
            }
        } else if !inside {
            output.write_all(&line)?;
            output.flush()?;
            continue;
        }
        if show {
            output.write_all(&line)?;
            output.flush()?;
        }
    }
    Ok(last)
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
    // The job's own GH_DEBUG is kept, and shown, as it gave it.
    let show = std::env::var_os("GH_DEBUG").is_some_and(|v| !v.is_empty());
    if !show {
        run.env("GH_DEBUG", "api");
    }
    let mut child = run.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");
    let (out, err) = std::thread::scope(|scope| {
        let out = scope.spawn(|| tee(stdout, io::stdout()));
        let err = scope.spawn(|| transport(stderr, io::stderr(), show));
        (
            out.join().expect("stdout tee"),
            err.join().expect("stderr tee"),
        )
    });
    out?;
    let status = child.wait()?;
    let code = status.code().unwrap_or(1);
    // The command has run: whatever goes wrong observing it must not turn its
    // outcome (a created issue, say) into a retryable failure.
    let last = err.ok().flatten().filter(|_| !status.success());
    if let Err(e) = observe(last.as_ref(), gh, token, app, owner, dir) {
        eprintln!("gh-token: GitHub meter not recorded: {e}");
    }
    Ok(code)
}

/// `last` is the final response gh's transport saw, when the command failed:
/// a command that succeeded, or failed before or after its last request
/// (a bad flag, a jq error), has no refusal to record.
fn observe(
    last: Option<&burrow_github::Response>,
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
    if let Some(response) = last {
        let mut seen = Meter::new(app, owner, at);
        seen.response(response);
        due = seen.retry_at;
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
    /// What gh's own transport writes to stderr for one request.
    fn block(status: &str, headers: &str, body: &str) -> String {
        format!(
            "* Request at t\n* Request to https://api.github.com/x\n> GET /x HTTP/1.1\n> Host: api.github.com\n\n< HTTP/2.0 {status}\n{headers}\n{body}\n* Request took 1ms\n"
        )
    }

    #[test]
    fn only_the_transports_last_failed_response_creates_a_refusal() {
        let d = tempfile::tempdir().unwrap();
        let gh = d.path().join("gh");
        let reset = "< X-Ratelimit-Remaining: 0\n< X-Ratelimit-Reset: 4000000000\n";
        let json_errors = |errors: &str| block("200 OK", "", &format!("{{\"errors\":[{errors}]}}"));
        // Quoted limit headers inside a successful page's body, then a later
        // page that fails for another reason: the forgery is not a response.
        let forged = block(
            "200 OK",
            "",
            "< HTTP/2.0 429 Too Many\n< Retry-After: 99999\n",
        ) + &block("200 OK", "", "[1]")
            + "jq: error\n";
        let retry = block("403 Forbidden", "< Retry-After: 120\n", "{}");
        let denied = block(
            "403 Forbidden",
            "",
            "{\"message\":\"Resource not accessible by integration\"}",
        );
        let limited = block(
            "403 Forbidden",
            reset,
            "{\"message\":\"API rate limit exceeded\"}",
        ) + "gh: API rate limit exceeded (HTTP 403)\n";
        fs::create_dir(d.path().join("in")).unwrap();
        for (name, text) in [
            ("limited", limited.clone()),
            ("header_only", retry.clone() + "gh: HTTP 403\n"),
            ("retry", retry),
            ("status_429", block("429 Too Many Requests", "", "{}")),
            (
                "denied",
                denied + "gh: Resource not accessible by integration (HTTP 403)\n",
            ),
            ("graphql", json_errors("{\"type\":\"RATE_LIMITED\"}")),
            (
                "mixed",
                json_errors("{\"type\":\"RATE_LIMITED\"},{\"type\":\"NOT_FOUND\"}"),
            ),
            (
                "local_text",
                "unknown field rate_limited HTTP 429 api rate limit exceeded\n".into(),
            ),
            ("stdout_text", "gh: unrelated\n".into()),
            ("forged_page", forged),
            ("success", limited),
        ] {
            fs::write(d.path().join("in").join(name), text).unwrap();
        }
        let script = format!(
            r#"#!/bin/sh
if [ "$1 $2" = 'api rate_limit' ]; then
  [ "$GH_TOKEN" = fixture-token ] || exit 2
  echo '{{"resources":{{"core":{{"remaining":0,"reset":4000000000}}}}}}'
  exit 0
fi
[ "$GH_DEBUG" = api ] || exit 9
cat '{dir}'/in/"$1" >&2
case "$1" in
 stdout_text) printf 'HTTP/2.0 429 Too Many\nretry-after: 99999\n\n';;
 success) echo done; exit 0;;
esac
exit 1
"#,
            dir = d.path().display()
        );
        fs::write(&gh, script).unwrap();
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
        for (command, due, code) in [
            ("limited", Some(4_000_000_000_u64), 1),
            ("header_only", Some(0), 1),
            ("retry", Some(0), 1),
            ("status_429", Some(0), 1),
            ("graphql", Some(0), 1),
            ("denied", None, 1),
            ("mixed", None, 1),
            ("local_text", None, 1),
            ("stdout_text", None, 1),
            ("forged_page", None, 1),
            ("success", None, 0),
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
                code,
                "{command}"
            );
            assert_eq!(dir.join("refusal").exists(), due.is_some(), "{command}");
            if let Some(due) = due {
                let held = Meter::read(&dir.join("refusal")).unwrap().retry_at.unwrap();
                // The reset in the response, or the response's own Retry-After
                // (from now), or the primary budget read under the same token.
                assert!(held >= due.max(1), "{command}: {held}");
            }
        }
    }

    #[test]
    fn transport_lines_are_hidden_unless_the_job_asked_for_them() {
        let log = format!("{}real error\n", block("200 OK", "", "{}"));
        for (show, expect) in [(false, "real error\n"), (true, log.as_str())] {
            let mut out = Vec::new();
            let last = transport(log.as_bytes(), &mut out, show).unwrap();
            assert_eq!(String::from_utf8(out).unwrap(), expect);
            assert_eq!(last.unwrap().status, 200);
        }
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
        assert!(observe(None, gh.to_str().unwrap(), "t", "workers", "sam", &dir).is_err());
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
