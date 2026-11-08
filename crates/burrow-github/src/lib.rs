//! GitHub's REST API, through gh: it brings the login (GH_TOKEN, GitHub
//! Actions' token for one, or gh's own), the proxy and the host. burrow's
//! programs that talk to GitHub share it; with the `fake` feature, their
//! tests get a GitHub that answers from a script.

use std::{fmt, thread, time::Duration};

use serde_json::Value;

/// Allocation bound for deadline-bound GitHub reads, including pagination.
pub const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

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

/// Every JSON page of a REST read, following GitHub's Link header. Each
/// page goes through the same retries; a failed page fails the whole read.
pub fn pages(api: &mut dyn Api, path: &str) -> Result<Vec<Value>, Error> {
    pages_with_limit(api, path, usize::MAX)
}

/// As `pages`, with a cumulative byte limit on the JSON pages and their
/// enclosing array. Reject the whole read before retaining an excess page.
pub fn pages_with_limit(
    api: &mut dyn Api,
    path: &str,
    max_bytes: usize,
) -> Result<Vec<Value>, Error> {
    let mut path = path.to_owned();
    let mut pages = Vec::new();
    let mut bytes = 2usize;
    loop {
        let response = call(api, "GET", &path, None, None)?;
        bytes = bytes
            .checked_add(response.body.len())
            .and_then(|n| n.checked_add(usize::from(!pages.is_empty())))
            .filter(|n| *n <= max_bytes)
            .ok_or_else(|| {
                Error::said(format!(
                    "GET {path}: paginated response exceeded {max_bytes} bytes"
                ))
            })?;
        pages.push(response.json()?);
        match response.next() {
            Some(next) => path = next,
            None => return Ok(pages),
        }
    }
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

mod transport;
pub use transport::{Gh, parse};

#[cfg(any(test, feature = "fake"))]
pub mod fake;

mod repo;
pub use repo::repo_name;

#[cfg(test)]
mod tests;
