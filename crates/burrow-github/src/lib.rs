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

/// A GraphQL query about `owner`'s account (Api::graphql): its `data`,
/// tried again as a GET is, since a query changes nothing (a mutation isn't
/// one), and an Error when GitHub's answer holds errors.
pub fn query(
    api: &mut dyn Api,
    owner: &str,
    query: &str,
    variables: Value,
) -> Result<Value, Error> {
    let body = serde_json::json!({ "query": query, "variables": variables });
    let what = "POST graphql";
    let r = kept(
        what,
        tried(api, what, true, |api| api.graphql(owner, &body))?,
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
            Err(f) => {
                let why = format!("{what}: {}", f.why);
                return Err(if f.transient {
                    Error::unanswered(why)
                } else {
                    Error::said(why)
                });
            }
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
    let mut cmd = match gh.via.split_first() {
        Some((under, args)) => {
            let mut c = Command::new(under);
            c.args(args).arg("gh");
            c
        }
        None => Command::new("gh"),
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
pub mod fake {
    //! A GitHub that answers from a script and keeps what it was asked.

    use super::*;
    use std::collections::VecDeque;

    type Answers = VecDeque<Result<Response, Failed>>;
    /// What it was asked: method, path, body, etag.
    pub type Asked = (String, String, Option<Value>, Option<String>);

    #[derive(Default)]
    pub struct Fake {
        /// A method and a path's start, and its answers in order; the last
        /// one stays.
        pub answers: Vec<(String, String, Answers)>,
        pub asked: Vec<Asked>,
        pub paused: Vec<u64>,
    }

    pub fn ok(status: u16, body: Value, headers: &[(&str, &str)]) -> Result<Response, Failed> {
        Ok(Response {
            status,
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            body: if body.is_null() {
                String::new()
            } else {
                body.to_string()
            },
        })
    }

    impl Fake {
        pub fn on(
            &mut self,
            method: &str,
            path: &str,
            answers: Vec<Result<Response, Failed>>,
        ) -> &mut Self {
            self.answers
                .push((method.into(), path.into(), answers.into()));
            self
        }

        pub fn asked(&self, method: &str) -> Vec<&Asked> {
            self.asked.iter().filter(|a| a.0 == method).collect()
        }
    }

    impl Api for Fake {
        fn request(
            &mut self,
            method: &str,
            path: &str,
            body: Option<&Value>,
            etag: Option<&str>,
        ) -> Result<Response, Failed> {
            self.asked.push((
                method.into(),
                path.into(),
                body.cloned(),
                etag.map(String::from),
            ));
            let (_, _, q) = self
                .answers
                .iter_mut()
                .find(|(m, p, _)| m == method && path.starts_with(p.as_str()))
                .unwrap_or_else(|| panic!("nothing scripted for {method} {path}"));
            if q.len() > 1 {
                q.pop_front().unwrap()
            } else {
                match q.front().unwrap() {
                    Ok(r) => Ok(r.clone()),
                    Err(f) => Err(Failed {
                        why: f.why.clone(),
                        transient: f.transient,
                    }),
                }
            }
        }

        fn pause(&mut self, secs: u64) {
            self.paused.push(secs);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{Fake, ok};
    use super::*;
    use serde_json::json;

    #[test]
    fn encodes_a_query_value() {
        assert_eq!(encode("updates"), "updates");
        assert_eq!(encode("needs peter&co"), "needs%20peter%26co");
        assert_eq!(encode("é"), "%C3%A9");
        assert_eq!(encode("repo:a/b \"x\""), "repo%3Aa%2Fb%20%22x%22");
    }

    #[test]
    fn reads_gh() {
        let r = parse("HTTP/2.0 200 OK\r\nEtag: W/\"abc\"\r\nLink: <https://api.github.com/repositories/1/issues?page=2>; rel=\"next\", <https://api.github.com/repositories/1/issues?page=3>; rel=\"last\"\r\n\r\n[1]").unwrap();
        assert_eq!(
            (r.status, r.header("ETAG"), r.body.as_str()),
            (200, Some("W/\"abc\""), "[1]")
        );
        assert_eq!(r.next().as_deref(), Some("repositories/1/issues?page=2"));
        let r = parse("HTTP/2.0 304 Not Modified\nEtag: x\n").unwrap();
        assert_eq!((r.status, r.body.as_str(), r.next()), (304, "", None));
        assert_eq!(r.header("etag"), Some("x"));
        // What gh really prints: \n after the status, \r\n after headers, and
        // a body that can hold either blank line itself.
        let r = parse(
            "HTTP/1.1 200 OK\nEtag: \"e\"\r\nServer: github.com\r\n\r\n[\"a\\n\\nb\", \"\r\n\r\n\"]",
        )
        .unwrap();
        assert_eq!(
            (r.header("etag"), r.header("server"), r.body.as_str()),
            (
                Some("\"e\""),
                Some("github.com"),
                "[\"a\\n\\nb\", \"\r\n\r\n\"]"
            )
        );
        assert!(parse("").is_none());
        assert!(parse("error connecting to api.github.com").is_none());
    }

    #[test]
    fn tries_again_on_what_passes() {
        let mut gh = Fake::default();
        gh.on(
            "GET",
            "a",
            vec![
                Err(Failed {
                    why: "gh: error connecting".into(),
                    transient: true,
                }),
                ok(502, Value::Null, &[]),
                ok(200, json!([1]), &[]),
            ],
        );
        assert_eq!(call(&mut gh, "GET", "a", None, None).unwrap().body, "[1]");
        assert_eq!(gh.paused, vec![2, 8]);

        let mut gh = Fake::default();
        gh.on("GET", "a", vec![ok(503, Value::Null, &[])]);
        assert_eq!(
            call(&mut gh, "GET", "a", None, None).unwrap_err().why,
            "GET a: HTTP 503, 3 times"
        );
        assert_eq!(gh.asked.len(), 3);
    }

    #[test]
    fn answers_what_the_caller_reads() {
        let mut gh = Fake::default();
        gh.on(
            "GET",
            "c",
            vec![ok(404, json!({"message": "Not Found"}), &[])],
        );
        let r = call_allowing(&mut gh, "GET", "c", None, None, &[404]).unwrap();
        assert_eq!((r.status, gh.asked.len()), (404, 1));
        // Only those: another refusal is still an Error, and so is the same
        // one to a plain call.
        assert_eq!(
            call(&mut gh, "GET", "c", None, None).unwrap_err().why,
            "GET c: HTTP 404: Not Found"
        );
        assert!(call_allowing(&mut gh, "GET", "c", None, None, &[422]).is_err());
        // What's worth asking again still is.
        let mut gh = Fake::default();
        gh.on(
            "GET",
            "c",
            vec![ok(502, Value::Null, &[]), ok(404, json!({}), &[])],
        );
        let r = call_allowing(&mut gh, "GET", "c", None, None, &[404]).unwrap();
        assert_eq!((r.status, gh.paused.as_slice()), (404, &[2][..]));
    }

    /// A GitHub that keeps whose account each GraphQL call was about.
    struct Graph {
        asked: Vec<(String, Value)>,
        answers: Vec<Result<Response, Failed>>,
    }

    impl Api for Graph {
        fn request(
            &mut self,
            method: &str,
            path: &str,
            _: Option<&Value>,
            _: Option<&str>,
        ) -> Result<Response, Failed> {
            panic!("{method} {path} isn't GraphQL")
        }

        fn graphql(&mut self, owner: &str, body: &Value) -> Result<Response, Failed> {
            self.asked.push((owner.into(), body.clone()));
            self.answers.remove(0)
        }

        fn pause(&mut self, _: u64) {}
    }

    #[test]
    fn queries_as_the_account_they_are_about() {
        let mut gh = Graph {
            asked: vec![],
            answers: vec![
                ok(502, Value::Null, &[]),
                ok(200, json!({"data": {"viewer": {"login": "sam"}}}), &[]),
            ],
        };
        let data = query(
            &mut gh,
            "Sam",
            "query { viewer { login } }",
            json!({"a": 1}),
        )
        .unwrap();
        assert_eq!(data, json!({"viewer": {"login": "sam"}}));
        assert_eq!(gh.asked.len(), 2);
        assert_eq!(
            gh.asked[1],
            (
                "Sam".into(),
                json!({"query": "query { viewer { login } }", "variables": {"a": 1}})
            )
        );
        // An answer that holds errors is an Error, whatever its status.
        let mut gh = Graph {
            asked: vec![],
            answers: vec![ok(
                200,
                json!({"data": {"repositoryOwner": null}, "errors": [
                    {"message": "Could not resolve to a RepositoryOwner with the login of 'nobody'."},
                    {"message": "and more"}
                ]}),
                &[],
            )],
        };
        assert_eq!(
            query(&mut gh, "nobody", "q", json!({})).unwrap_err().why,
            "POST graphql: Could not resolve to a RepositoryOwner with the login of 'nobody'.; and more"
        );
        let mut gh = Graph {
            asked: vec![],
            answers: vec![ok(401, json!({"message": "Bad credentials"}), &[])],
        };
        assert_eq!(
            query(&mut gh, "sam", "q", json!({})).unwrap_err().why,
            "POST graphql: HTTP 401: Bad credentials"
        );
        // Asked of a GitHub with nothing of its own for it, it's a POST.
        let mut gh = Fake::default();
        gh.on("POST", "graphql", vec![ok(200, json!({"data": 1}), &[])]);
        assert_eq!(query(&mut gh, "sam", "q", json!({})).unwrap(), json!(1));
        assert_eq!(gh.asked[0].1, "graphql");
    }

    #[test]
    fn asks_a_post_again_only_when_github_did_nothing() {
        // GitHub can have made the issue when its answer is lost or a 5xx:
        // asking again would make a second.
        for (answer, said) in [
            (
                Err(Failed {
                    why: "gh: connection reset by peer".into(),
                    transient: true,
                }),
                "POST b: gh: connection reset by peer",
            ),
            (ok(502, Value::Null, &[]), "POST b: HTTP 502: "),
        ] {
            let mut gh = Fake::default();
            gh.on("POST", "b", vec![answer]);
            let e = call(&mut gh, "POST", "b", Some(&json!({})), None).unwrap_err();
            assert_eq!(e.why, said);
            assert_eq!(gh.asked.len(), 1);
            assert!(gh.paused.is_empty());
        }
        // A 429 is refused before anything's done.
        let mut gh = Fake::default();
        gh.on(
            "POST",
            "b",
            vec![
                ok(429, Value::Null, &[]),
                ok(201, json!({"number": 7}), &[]),
            ],
        );
        let r = call(&mut gh, "POST", "b", Some(&json!({})), None).unwrap();
        assert_eq!(r.status, 201);
        assert_eq!(gh.paused, vec![2]);
        // What asking again can't do twice still is.
        for method in ["PATCH", "PUT", "DELETE"] {
            let mut gh = Fake::default();
            gh.on(
                method,
                "c",
                vec![ok(502, Value::Null, &[]), ok(200, json!({}), &[])],
            );
            assert_eq!(call(&mut gh, method, "c", None, None).unwrap().status, 200);
        }
    }

    #[test]
    fn gives_up_on_what_wont() {
        let mut gh = Fake::default();
        gh.on(
            "POST",
            "b",
            vec![ok(
                403,
                json!({"message": "Resource not accessible by integration"}),
                &[],
            )],
        );
        let e = call(&mut gh, "POST", "b", Some(&json!({})), None).unwrap_err();
        assert_eq!(
            e.why,
            "POST b: HTTP 403: Resource not accessible by integration"
        );
        assert_eq!((gh.asked.len(), gh.paused.len()), (1, 0));

        let mut gh = Fake::default();
        gh.on(
            "GET",
            "b",
            vec![Err(Failed {
                why: "gh: To get started with GitHub CLI, please run: gh auth login".into(),
                transient: false,
            })],
        );
        assert!(
            call(&mut gh, "GET", "b", None, None)
                .unwrap_err()
                .why
                .contains("gh auth login")
        );
        assert_eq!(gh.asked.len(), 1);
        assert!(
            Failed::from_gh("dial tcp: i/o timeout".into())
                .unwrap_err()
                .transient
        );
        assert!(
            !Failed::from_gh("HTTP 401: Bad credentials".into())
                .unwrap_err()
                .transient
        );
        // The URL gh quotes is the call's, not what went wrong.
        assert!(
            !Failed::from_gh(
                "Get \"https://api.github.com/repos/sam/timeout/git/ref/eof\": invalid header"
                    .into()
            )
            .unwrap_err()
            .transient
        );
    }

    #[test]
    fn says_whether_github_answered_by_the_status_never_the_words() {
        // Every answer names the repo `timeout` and the branch `eof`, as a
        // path and as GitHub's message: neither makes an outage.
        let path = "repos/sam/timeout/compare/main...eof";
        let message = json!({"message": "No common ancestor between main and eof: timeout"});
        for (answer, method, unanswered) in [
            (ok(404, message.clone(), &[]), "GET", false),
            (ok(422, message.clone(), &[]), "POST", false),
            (ok(403, message.clone(), &[]), "GET", false),
            (ok(503, message.clone(), &[]), "GET", true),
            (ok(502, message.clone(), &[]), "POST", true),
            (ok(429, message.clone(), &[]), "GET", true),
            (
                ok(403, message.clone(), &[("x-ratelimit-remaining", "0")]),
                "GET",
                true,
            ),
            (
                ok(403, message.clone(), &[("retry-after", "60")]),
                "GET",
                true,
            ),
            (
                ok(
                    403,
                    json!({"message": "You have exceeded a secondary rate limit"}),
                    &[],
                ),
                "GET",
                true,
            ),
            (
                Err(Failed {
                    why: "gh: connection reset by peer".into(),
                    transient: true,
                }),
                "GET",
                true,
            ),
            (
                Err(Failed {
                    why: "gh: connection reset by peer".into(),
                    transient: true,
                }),
                "POST",
                true,
            ),
            (
                Err(Failed {
                    why: format!("gh: Get \"https://api.github.com/{path}\": bad"),
                    transient: false,
                }),
                "GET",
                false,
            ),
        ] {
            let mut gh = Fake::default();
            gh.on(method, path, vec![answer]);
            let e = call(&mut gh, method, path, None, None).unwrap_err();
            assert_eq!(e.unanswered, unanswered, "{e}");
        }
        // An answer that won't do is GitHub's.
        let mut gh = Fake::default();
        gh.on("GET", path, vec![ok(200, Value::Null, &[])]);
        let r = call(&mut gh, "GET", path, None, None).unwrap();
        assert!(!r.json().unwrap_err().unanswered);
    }

    /// A Gh whose gh is run under a stand-in for runuser, which answers as
    /// gh would, with what it was run as: its words, and the environment's
    /// `shows`.
    fn under(shows: &str) -> Gh {
        let answer =
            format!(r#"cat >/dev/null; printf 'HTTP/1.1 200 OK\n\n%s|%s' "$*" "${{{shows}-}}""#);
        Gh {
            token: None,
            via: ["/bin/sh", "-c", &answer, "runuser"]
                .map(OsString::from)
                .to_vec(),
        }
    }

    #[test]
    fn runs_gh_under_its_via() {
        let mut gh = under("GH_TOKEN_OWNER");
        let r = gh.request("GET", "repos/sam/homelab", None, None).unwrap();
        assert_eq!(r.body, "gh api -i -X GET repos/sam/homelab|");
        // GraphQL as the account it's about, through whatever runs gh.
        let r = gh.graphql("sam", &json!({"query": "q"})).unwrap();
        assert_eq!(r.body, "gh api -i -X POST graphql --input -|sam");
        let mut gh = Gh {
            token: Some("ghs_x".into()),
            ..under("GH_TOKEN")
        };
        let r = gh.request("GET", "a", None, Some("e")).unwrap();
        assert_eq!(r.body, "gh api -i -X GET a -H If-None-Match: e|ghs_x");
    }
}
