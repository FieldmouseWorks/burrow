use super::{Api, Failed, MAX_RESPONSE_BYTES, Response};
use serde_json::Value;
use std::{
    ffi::OsString,
    io::Write,
    process::{Command, Stdio},
    thread,
    time::Duration,
};

/// gh api, with -i so the status and headers come back too: as the login
/// it's run with, or with `token` when it's given one.
#[derive(Default)]
pub struct Gh {
    /// Executable to run; None uses gh from PATH.
    pub program: Option<OsString>,
    /// Authentication routing and the configured App environment.
    pub env: Vec<(OsString, OsString)>,
    /// Absolute deadline shared across requests and retry waits.
    pub until: Option<std::time::Instant>,
    /// What every call goes with (GH_TOKEN) in place of that login: an
    /// App's installation token a program minted itself.
    pub token: Option<String>,
    /// The command gh is run under, when it's to run as another user than
    /// this program (`runuser -u USER --`, for one root runs that acts as
    /// the machine's user): it runs `gh ...` itself, in this program's
    /// environment. Empty, gh is run as this program runs.
    pub via: Vec<OsString>,
}

impl Api for Gh {
    fn request(
        &mut self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        etag: Option<&str>,
    ) -> Result<Response, Failed> {
        gh_api(self, method, path, body, etag, None)
    }

    fn pause(&mut self, secs: u64) {
        let wait = Duration::from_secs(secs);
        thread::sleep(self.until.map_or(wait, |u| {
            wait.min(u.saturating_duration_since(std::time::Instant::now()))
        }));
    }

    fn graphql(&mut self, owner: &str, body: &Value) -> Result<Response, Failed> {
        gh_api(self, "POST", "graphql", Some(body), None, Some(owner))
    }
}

/// `gh api -i` under gh's `via`, with its `token` when there is one, else
/// as `owner`'s account when the call names none.
fn gh_api(
    gh: &Gh,
    method: &str,
    path: &str,
    body: Option<&Value>,
    etag: Option<&str>,
    owner: Option<&str>,
) -> Result<Response, Failed> {
    let mut cmd = match gh.via.split_first() {
        Some((under, args)) => {
            let mut c = Command::new(under);
            c.args(args)
                .arg(gh.program.as_deref().unwrap_or(std::ffi::OsStr::new("gh")));
            c
        }
        None => Command::new(gh.program.as_deref().unwrap_or(std::ffi::OsStr::new("gh"))),
    };
    cmd.envs(gh.env.iter().cloned());
    cmd.args(["api", "-i", "-X", method, path]);
    if let Some(t) = &gh.token {
        cmd.env("GH_TOKEN", t);
    } else if let Some(owner) = owner {
        // gh-token goes by GH_REPO and the checkout's remotes before this
        // (account.rs), and they're not what the call is about.
        cmd.env("GH_TOKEN_OWNER", owner)
            .env_remove("GH_REPO")
            .env_remove("GIT_DIR")
            .current_dir("/");
    }
    if let Some(tag) = etag {
        cmd.args(["-H", &format!("If-None-Match: {tag}")]);
    }
    if body.is_some() {
        cmd.args(["--input", "-"]);
    }
    cmd.stdin(if body.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    if let Some(until) = gh.until {
        let input = body.map(Value::to_string);
        let out = burrow_process::output(
            &mut cmd,
            input.as_deref().map(str::as_bytes),
            until,
            MAX_RESPONSE_BYTES,
        )
        .map_err(|why| Failed {
            why,
            transient: false,
        })?;
        return match parse(&String::from_utf8_lossy(&out.stdout)) {
            Some(r) => Ok(r),
            None => Failed::from_gh(String::from_utf8_lossy(&out.stderr).trim().to_owned()),
        };
    }
    let mut child = cmd.spawn().map_err(|e| Failed {
        why: format!("gh: {e}"),
        transient: false,
    })?;
    if let Some(b) = body {
        let mut stdin = child.stdin.take().expect("piped");
        stdin
            .write_all(b.to_string().as_bytes())
            .map_err(|e| Failed {
                why: format!("gh: {e}"),
                transient: true,
            })?;
    }
    let out = child.wait_with_output().map_err(|e| Failed {
        why: format!("gh: {e}"),
        transient: true,
    })?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    match parse(&stdout) {
        Some(r) => Ok(r),
        None => {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            Failed::from_gh(err)
        }
    }
}

impl Failed {
    pub(crate) fn from_gh(err: String) -> Result<Response, Failed> {
        // Less the URL gh quotes (`Get "https://...": dial tcp ...`), whose
        // path names whatever the call was about.
        let lower = unquoted(&err).to_lowercase();
        let transient = [
            "connect",
            "timeout",
            "timed out",
            "eof",
            "reset",
            "tls",
            "no such host",
            "unreachable",
            "temporary",
        ]
        .iter()
        .any(|w| lower.contains(w));
        Err(Failed {
            why: format!("gh: {err}"),
            transient,
        })
    }
}

/// `said`, less what's between double quotes.
fn unquoted(said: &str) -> String {
    said.split('"').step_by(2).collect::<Vec<_>>().join(" ")
}

/// gh api -i's output: the status line, headers, a blank line, the body.
/// None when it printed no response (it never got one).
pub fn parse(out: &str) -> Option<Response> {
    // gh ends the status line with \n and GitHub's headers with \r\n, so the
    // head ends at whichever blank line comes first.
    let end = [
        out.find("\r\n\r\n").map(|i| (i, 4)),
        out.find("\n\n").map(|i| (i, 2)),
    ]
    .into_iter()
    .flatten()
    .min();
    let (head, body) = end.map_or((out, ""), |(i, n)| (&out[..i], &out[i + n..]));
    let mut lines = head.lines().map(|l| l.trim_end_matches('\r'));
    let status_line = lines.next()?;
    if !status_line.starts_with("HTTP/") {
        return None;
    }
    let status = status_line.split_whitespace().nth(1)?.parse().ok()?;
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect();
    Some(Response {
        status,
        headers,
        body: body.to_string(),
    })
}
