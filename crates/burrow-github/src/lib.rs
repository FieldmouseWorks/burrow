//! GitHub's REST API, through gh: it brings the login (GH_TOKEN, GitHub
//! Actions' token for one, or gh's own), the proxy and the host. burrow's
//! programs that talk to GitHub share it; with the `fake` feature, their
//! tests get a GitHub that answers from a script.

use std::ffi::OsString;
use std::io::Write;
use std::process::{Command, Stdio};
use std::{fmt, thread, time::Duration};

use serde_json::Value;

#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn json(&self) -> Result<Value, Error> {
        serde_json::from_str(&self.body)
            .map_err(|e| Error::said(format!("GitHub's answer isn't JSON: {e}")))
    }

    /// The next page's path, from the Link header.
    pub fn next(&self) -> Option<String> {
        let link = self.header("link")?;
        let part = link.split(',').find(|p| p.contains("rel=\"next\""))?;
        let url = part.split('<').nth(1)?.split('>').next()?;
        Some(
            url.strip_prefix("https://api.github.com/")
                .unwrap_or(url)
                .to_string(),
        )
    }
}

/// What went wrong with a call: what GitHub said, or that it didn't answer.
#[derive(Debug, PartialEq, Eq)]
pub struct Error {
    pub why: String,
    /// GitHub didn't answer: a rate limit, a 5xx, no connection. Nothing
    /// asked for was wrong, and the same ask can go again later. Told by
    /// the status and the transport, never by the words, which name
    /// whatever the request was about (a repo called `timeout`).
    pub unanswered: bool,
}

impl Error {
    /// GitHub's no, or an answer that won't do.
    pub fn said(why: impl Into<String>) -> Self {
        Self {
            why: why.into(),
            unanswered: false,
        }
    }

    fn unanswered(why: String) -> Self {
        Self {
            why,
            unanswered: true,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.why)
    }
}

impl From<Error> for String {
    fn from(e: Error) -> String {
        e.why
    }
}

pub trait Api {
    /// One request, its body JSON. Ok is an answer, of any status; Err, none
    /// (with `transient` when it's worth asking again).
    fn request(
        &mut self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        etag: Option<&str>,
    ) -> Result<Response, Failed>;

    /// One GraphQL request about `owner`'s account. It names no repo, so it
    /// says whose it is: an App's gh takes that account's installation's
    /// token (GH_TOKEN_OWNER, gh-token), and no repo around it says otherwise.
    fn graphql(&mut self, owner: &str, body: &Value) -> Result<Response, Failed> {
        let _ = owner;
        self.request("POST", "graphql", Some(body), None)
    }

    /// A wait before asking again.
    fn pause(&mut self, secs: u64) {
        thread::sleep(Duration::from_secs(secs));
    }
}

/// No answer at all.
#[derive(Debug)]
pub struct Failed {
    pub why: String,
    pub transient: bool,
}

/// The waits before the second and third tries.
const AGAIN: [u64; 2] = [2, 8];

/// A request that has to work: tried again after AGAIN on a 429, and on no
/// answer or a 5xx when asking again can't do it twice; 304 (with `etag`)
/// and 2xx are answers, anything else an Error that says what GitHub said.
/// A POST makes something each time (an issue, a comment), and GitHub can
/// have made it when its answer is lost or a 5xx, so a POST is asked again
/// only after a 429, which GitHub answers without doing anything.
pub fn call(
    api: &mut dyn Api,
    method: &str,
    path: &str,
    body: Option<&Value>,
    etag: Option<&str>,
) -> Result<Response, Error> {
    call_allowing(api, method, path, body, etag, &[])
}

/// call, where the statuses in `allowed` are answers too, for the caller to
/// read (a 404 for no such thing): never an Error, and never asked again.
pub fn call_allowing(
    api: &mut dyn Api,
    method: &str,
    path: &str,
    body: Option<&Value>,
    etag: Option<&str>,
    allowed: &[u16],
) -> Result<Response, Error> {
    let what = format!("{method} {path}");
    let r = tried(api, &what, method != "POST", |api| {
        api.request(method, path, body, etag)
    })?;
    kept(&what, r, allowed)
}

/// A write that asked again could undo someone else's (a PATCH of a PR's
/// description, which its owner edits too): asked once, whatever comes
/// back. One whose answer is lost or a 5xx may have been done, and after a
/// 429 anything may have changed while it waited, so what it was asked on
/// is the caller's to read again before it asks again.
pub fn call_once(
    api: &mut dyn Api,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Result<Response, Error> {
    let what = format!("{method} {path}");
    let r = api
        .request(method, path, body, None)
        .map_err(|f| failed(&what, f))?;
    kept(&what, r, &[])
}

/// A write that comes out the same however often it's made, asked again as
/// a GET is: a commit status, where the newest one for its context is the
/// one that counts, so a second copy of it changes nothing.
pub fn call_repeating(
    api: &mut dyn Api,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Result<Response, Error> {
    let what = format!("{method} {path}");
    let r = tried(api, &what, true, |api| {
        api.request(method, path, body, None)
    })?;
    kept(&what, r, &[])
}

/// A GraphQL query about `owner`'s account (Api::graphql): its `data`,
/// tried again as a GET is, since a query changes nothing (a mutation isn't
/// one), and an Error when GitHub's answer holds errors.
pub fn query(
    api: &mut dyn Api,
    owner: &str,
    query: &str,
    variables: Value,
) -> Result<Value, Error> {
    graphql(api, owner, query, variables, true)
}

/// A GraphQL mutation about `owner`'s account: its `data`, or an Error as
/// `query` gives one. Asked again only after a 429, as a POST is (`call`):
/// GitHub can have done it when its answer is lost or a 5xx.
pub fn mutate(
    api: &mut dyn Api,
    owner: &str,
    mutation: &str,
    variables: Value,
) -> Result<Value, Error> {
    graphql(api, owner, mutation, variables, false)
}

fn graphql(
    api: &mut dyn Api,
    owner: &str,
    query: &str,
    variables: Value,
    repeats: bool,
) -> Result<Value, Error> {
    let body = serde_json::json!({ "query": query, "variables": variables });
    let what = "POST graphql";
    let r = kept(
        what,
        tried(api, what, repeats, |api| api.graphql(owner, &body))?,
        &[],
    )?;
    let mut answer = r.json()?;
    if let Some(errors) = answer.get("errors").and_then(Value::as_array) {
        let said: Vec<&str> = errors
            .iter()
            .map(|e| e["message"].as_str().unwrap_or("an error"))
            .collect();
        let said: String = said.join("; ").chars().take(300).collect();
        return Err(Error::said(format!("{what}: {said}")));
    }
    Ok(answer["data"].take())
}

/// One request, asked again after AGAIN on a 429, and on no answer or a 5xx
/// when it `repeats`: the first answer that isn't one of those, whatever its
/// status.
fn tried(
    api: &mut dyn Api,
    what: &str,
    repeats: bool,
    mut ask: impl FnMut(&mut dyn Api) -> Result<Response, Failed>,
) -> Result<Response, Error> {
    let mut tries = 0;
    loop {
        let again = match ask(&mut *api) {
            Ok(r) if r.status == 429 || (repeats && r.status >= 500) => {
                format!("HTTP {}", r.status)
            }
            Ok(r) => return Ok(r),
            Err(f) if f.transient && repeats => f.why,
            Err(f) => return Err(failed(what, f)),
        };
        if tries == AGAIN.len() {
            return Err(Error::unanswered(format!(
                "{what}: {again}, {} times",
                tries + 1
            )));
        }
        api.pause(AGAIN[tries]);
        tries += 1;
    }
}

/// A request GitHub gave no answer to (`transient`: none came), or that
/// couldn't be made, as an Error.
fn failed(what: &str, f: Failed) -> Error {
    let why = format!("{what}: {}", f.why);
    if f.transient {
        Error::unanswered(why)
    } else {
        Error::said(why)
    }
}

/// An answer to keep: 2xx, 304 or one of `allowed`; anything else, an
/// Error that says what GitHub said. A 429, a 5xx (a POST's, not asked
/// again) and a 403 for a rate limit are GitHub not answering.
fn kept(what: &str, r: Response, allowed: &[u16]) -> Result<Response, Error> {
    if (200..300).contains(&r.status) || r.status == 304 || allowed.contains(&r.status) {
        return Ok(r);
    }
    let said = message(&r.body);
    let why = format!("{what}: HTTP {}: {said}", r.status);
    let limited = r.status == 403
        && (r.header("x-ratelimit-remaining") == Some("0")
            || r.header("retry-after").is_some()
            || said.to_lowercase().contains("rate limit"));
    Err(if r.status == 429 || r.status >= 500 || limited {
        Error::unanswered(why)
    } else {
        Error::said(why)
    })
}

fn message(body: &str) -> String {
    let m = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["message"].as_str().map(String::from));
    let m = m.unwrap_or_else(|| body.trim().to_string());
    m.chars().take(300).collect()
}

/// A value that goes into a path, as one part of it or in its query
/// (`?q=VALUE`): percent-encoded, all but the characters a URL never
/// reserves.
pub fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// gh api, with -i so the status and headers come back too: as the login
/// it's run with, or with `token` when it's given one.
#[derive(Default)]
pub struct Gh {
    /// An explicit CLI transport, independent of login wrappers on PATH.
    pub program: Option<OsString>,
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
    let program = gh
        .program
        .as_deref()
        .unwrap_or_else(|| std::ffi::OsStr::new("gh"));
    let mut cmd = match gh.via.split_first() {
        Some((under, args)) => {
            let mut c = Command::new(under);
            c.args(args).arg(program);
            c
        }
        None => Command::new(program),
    };
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
    fn from_gh(err: String) -> Result<Response, Failed> {
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

#[cfg(any(test, feature = "fake"))]
pub mod fake;

mod repo;
pub use repo::repo_name;

#[cfg(test)]
mod tests;
