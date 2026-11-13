//! GitHub's REST API, through gh: it brings the login (GH_TOKEN, GitHub
//! Actions' token for one, or gh's own), the proxy and the host. burrow's
//! programs that talk to GitHub share it; with the `fake` feature, their
//! tests get a GitHub that answers from a script.

use std::{fmt, thread, time::Duration};

use serde_json::Value;

mod author;
pub mod quota;
pub use author::{Author, author_names};

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

    /// The next page's path, from the Link header, for the answer to
    /// `asked`. GitHub links a repo's later pages by the repo's id
    /// (`repositories/ID/...`), which names no account, so gh-token can't
    /// tell whose token the call takes; the page is asked for by the
    /// `repos/OWNER/NAME` that `asked` names instead, the same list.
    pub fn next(&self, asked: &str) -> Option<String> {
        let link = self.header("link")?;
        let part = link.split(',').find(|p| p.contains("rel=\"next\""))?;
        let url = part.split('<').nth(1)?.split('>').next()?;
        let url = url.strip_prefix("https://api.github.com/").unwrap_or(url);
        Some(by_name(asked, url))
    }
}

/// `next` with a leading `repositories/ID` given as the `repos/OWNER/NAME`
/// that `asked` starts with; as it is when either isn't one.
fn by_name(asked: &str, next: &str) -> String {
    let Some(rest) = next.strip_prefix("repositories/") else {
        return next.to_owned();
    };
    let (id, tail) = rest.split_at(rest.find(['/', '?']).unwrap_or(rest.len()));
    let asked = asked.trim_start_matches('/');
    let asked = asked
        .strip_prefix("https://api.github.com/")
        .unwrap_or(asked);
    let mut part = asked.split(['/', '?']);
    match (part.next(), part.next(), part.next()) {
        (Some("repos"), Some(owner), Some(name))
            if !id.is_empty()
                && id.bytes().all(|b| b.is_ascii_digit())
                && !owner.is_empty()
                && !name.is_empty() =>
        {
            format!("repos/{owner}/{name}{tail}")
        }
        _ => next.to_owned(),
    }
}

/// What went wrong with a call: what GitHub said, or that it didn't answer.
#[derive(Debug, PartialEq, Eq)]
pub struct Error {
    pub why: String,
    /// `why` before GitHub's own words were cut to a few hundred
    /// characters (at most WHOLE_MAX), for a caller that must scrub a
    /// diagnostic before it shortens it: a cut can take the part of a
    /// credential that makes it one.
    pub whole: String,
    /// GitHub didn't answer: a rate limit, a 5xx, no connection. Nothing
    /// asked for was wrong, and the same ask can go again later. Told by
    /// the status and the transport, never by the words, which name
    /// whatever the request was about (a repo called `timeout`).
    pub unanswered: bool,
    /// Earliest retry time, only when every cause is a rate-limit failure.
    /// Kept through context and aggregation instead of parsing diagnostics.
    pub retry_at: Option<u64>,
}

impl Error {
    /// GitHub's no, or an answer that won't do.
    pub fn said(why: impl Into<String>) -> Self {
        let why = why.into();
        Self {
            whole: why.clone(),
            why,
            unanswered: false,
            retry_at: None,
        }
    }

    /// This error, its whole diagnostic being `whole`.
    fn in_full(mut self, whole: String) -> Self {
        self.whole = whole;
        self
    }

    fn unanswered(why: String) -> Self {
        Self {
            whole: why.clone(),
            why,
            unanswered: true,
            retry_at: None,
        }
    }
    /// Add context without losing the cause's retry classification.
    pub fn context(mut self, context: impl fmt::Display) -> Self {
        self.why = format!("{context}: {}", self.why);
        self.whole = format!("{context}: {}", self.whole);
        self
    }

    /// A collection is deferrable only when all its causes are rate limits.
    pub fn aggregate(errors: Vec<Self>) -> Self {
        let retry_at = errors
            .iter()
            .all(|e| e.retry_at.is_some())
            .then(|| errors.iter().filter_map(|e| e.retry_at).max())
            .flatten();
        Self {
            whole: errors
                .iter()
                .map(|e| e.whole.as_str())
                .collect::<Vec<_>>()
                .join("; "),
            why: errors
                .iter()
                .map(|e| e.why.as_str())
                .collect::<Vec<_>>()
                .join("; "),
            unanswered: !errors.is_empty() && errors.iter().all(|e| e.unanswered),
            retry_at,
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

impl From<String> for Error {
    fn from(why: String) -> Self {
        Self::said(why)
    }
}
impl From<&str> for Error {
    fn from(why: &str) -> Self {
        Self::said(why)
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

    /// Wall time after a response arrives; test APIs can advance a clock
    /// during requests, earlier pass work and retry waits.
    fn now(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
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
        match response.next(&path) {
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
    kept(&what, Received::new(r, api.now()), &[])
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
    let received = tried(api, what, repeats, |api| api.graphql(owner, &body))?;
    let at = received.at;
    let limited_until = received.limited_until;
    let r = kept(what, received, &[])?;
    let mut answer = r.json()?;
    if let Some(errors) = answer.get("errors").and_then(Value::as_array) {
        let said: Vec<&str> = errors
            .iter()
            .map(|e| e["message"].as_str().unwrap_or("an error"))
            .collect();
        let whole: String = said.join("; ").chars().take(WHOLE_MAX).collect();
        let why = format!("{what}: {}", cut(&whole));
        let whole = format!("{what}: {whole}");
        return Err(
            if !errors.is_empty() && errors.iter().all(graphql_rate_error) {
                limited(why, &r, at, limited_until)
            } else {
                Error::said(why)
            }
            .in_full(whole),
        );
    }
    Ok(answer["data"].take())
}

struct Received {
    response: Response,
    at: u64,
    limited_until: Option<u64>,
}
impl Received {
    fn new(response: Response, at: u64) -> Self {
        let limited_until = rate_limited(&response).then(|| retry_at(&response, at));
        Self {
            response,
            at,
            limited_until,
        }
    }
}

/// One request, asked again after AGAIN on a 429, and on no answer or a 5xx
/// when it `repeats`: the first answer that isn't one of those, whatever its
/// status.
fn tried(
    api: &mut dyn Api,
    what: &str,
    repeats: bool,
    mut ask: impl FnMut(&mut dyn Api) -> Result<Response, Failed>,
) -> Result<Received, Error> {
    let mut tries = 0;
    let mut limited_until = None;
    loop {
        let received = ask(&mut *api).map(|r| Received::new(r, api.now()));
        if let Ok(r) = &received {
            limited_until = limited_until.max(r.limited_until);
        }
        let again = match received {
            Ok(r) if r.response.status == 429 || (repeats && r.response.status >= 500) => {
                if tries == AGAIN.len() && r.response.status == 429 {
                    return Err(limited(
                        format!("{what}: HTTP 429, {} times", tries + 1),
                        &r.response,
                        r.at,
                        limited_until,
                    ));
                }
                format!("HTTP {}", r.response.status)
            }
            Ok(mut r) => {
                r.limited_until = limited_until;
                return Ok(r);
            }
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
fn kept(what: &str, received: Received, allowed: &[u16]) -> Result<Response, Error> {
    let Received {
        response: r,
        at,
        limited_until,
    } = received;
    if (200..300).contains(&r.status) || r.status == 304 || allowed.contains(&r.status) {
        return Ok(r);
    }
    let whole = message(&r.body);
    let why = format!("{what}: HTTP {}: {}", r.status, cut(&whole));
    let whole = format!("{what}: HTTP {}: {whole}", r.status);
    Err(if rate_limited(&r) {
        limited(why, &r, at, limited_until)
    } else if r.status >= 500 {
        Error::unanswered(why)
    } else {
        Error::said(why)
    }
    .in_full(whole))
}

fn rate_limited(r: &Response) -> bool {
    r.status == 429
        || r.status == 403
            && (r.header("x-ratelimit-remaining") == Some("0")
                || r.header("retry-after").is_some()
                || message(&r.body).to_lowercase().contains("rate limit"))
}

fn graphql_rate_error(error: &Value) -> bool {
    let kind = error["type"]
        .as_str()
        .or_else(|| error["extensions"]["code"].as_str());
    if let Some(kind) = kind {
        return kind == "RATE_LIMITED";
    }
    let text = error["message"].as_str().unwrap_or("").to_lowercase();
    text.contains("rate limit exceeded") || text.contains("exceeded a secondary rate limit")
}

fn retry_at(r: &Response, at: u64) -> u64 {
    let retry = r
        .header("retry-after")
        .and_then(|v| v.parse::<u64>().ok())
        .map(|delay| at.saturating_add(delay));
    let reset = (r.header("x-ratelimit-remaining") == Some("0"))
        .then(|| {
            r.header("x-ratelimit-reset")
                .and_then(|v| v.parse::<u64>().ok())
        })
        .flatten();
    retry
        .into_iter()
        .chain(reset)
        .filter(|deadline| *deadline > at)
        .max()
        .unwrap_or(at.saturating_add(60))
}

fn limited(why: String, r: &Response, at: u64, earlier: Option<u64>) -> Error {
    Error {
        whole: why.clone(),
        why,
        unanswered: true,
        retry_at: Some(retry_at(r, at).max(earlier.unwrap_or(0))),
    }
}

fn message(body: &str) -> String {
    let m = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["message"].as_str().map(String::from));
    let m = m.unwrap_or_else(|| body.trim().to_string());
    m.chars().take(WHOLE_MAX).collect()
}

/// The most of GitHub's words kept whole in an Error.
const WHOLE_MAX: usize = 64 * 1024;

/// GitHub's words as an Error's `why` says them.
fn cut(whole: &str) -> String {
    whole.chars().take(300).collect()
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

pub mod deferred;

mod transport;
pub use transport::{Gh, parse};

#[cfg(any(test, feature = "fake"))]
pub mod fake;

mod repo;
pub use repo::repo_name;

#[cfg(test)]
mod tests;
