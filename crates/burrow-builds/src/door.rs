//! The door: one upload a connection, from nginx, on stdin and stdout
//! (systemd's `Accept=yes`). It admits a job by Conary's rule, asked of `cn
//! attest job` and never copied here, then only a public repo the machine
//! lists, and hands the body to [`Store::upload`].
//!
//! Before nginx takes an upload's body it asks [`serve_admit`] (its
//! `auth_request`, on a socket of its own) whether the head's token may
//! write at all: the same [`admit`], asked with the audience the token
//! claims, since the manifest it must name is in the body. So a client
//! without a job's token is refused before nginx buffers a byte, and the
//! door still holds the token to the manifest's audience once it has it.

use std::io::{self, BufRead, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde::Deserialize;
use serde_json::{Value, json};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use crate::{Key, Manifest, Stopped, Store, audience, is_audience};

/// What the module tells the door.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The cache nginx serves.
    pub dir: PathBuf,
    /// The machine's signing key, which nginx never serves.
    pub key: PathBuf,
    /// The key's name, as a store that trusts it names it.
    pub key_name: String,
    /// The most bytes the cache's NARs and narinfos may take.
    pub max_size: u64,
    /// The repos whose runs may write it, as OWNER/NAME.
    pub repos: Vec<String>,
    /// `cn`, and what goes before its arguments: the machine's, which
    /// knows the signer workflow, its fresh runners and how to ask GitHub.
    pub cn: Vec<String>,
}

/// The most a request's line and headers, or its manifest, may take.
const MOST_HEAD: u64 = 64 * 1024;
const MOST_MANIFEST: u64 = 8 * 1024 * 1024;

/// An answer: its status and what it says.
#[derive(Debug, PartialEq)]
pub struct Answer {
    pub status: u16,
    pub body: Value,
}

fn answer(status: u16, body: Value) -> Answer {
    Answer { status, body }
}

fn refused(status: u16, why: impl Into<String>) -> Answer {
    answer(status, json!({ "refused": why.into() }))
}

/// What a request without a token is told.
const NO_TOKEN: &str = "an upload carries its job's token, as Authorization: Bearer";

/// The most bytes of a refusal [`serve_admit`] puts in its header, well
/// inside what nginx takes of an upstream's head (proxy_buffer_size).
const MOST_REASON: usize = 1024;

/// Serves one upload from `input`, answering on `output`.
pub fn serve(cfg: &Config, input: &mut dyn BufRead, output: &mut dyn Write) -> io::Result<()> {
    write_answer(output, &handle(cfg, input), "")
}

/// Serves one header-time check from `input`, nginx's `auth_request` before
/// it takes an upload's body: 200 when the head's token may write, or the
/// refusal [`admit`] gives. nginx drops a subrequest's body, so the refusal
/// rides a header too (`Burrow-Refused`), for nginx to answer the client.
pub fn serve_admit(
    cfg: &Config,
    input: &mut dyn BufRead,
    output: &mut dyn Write,
) -> io::Result<()> {
    let a = check(cfg, input);
    let header = if a.status == 200 {
        String::new()
    } else {
        // JSON escapes every control character: it stays one header line,
        // cut to fit as it's sent.
        let mut why: Vec<char> = a.body["refused"]
            .as_str()
            .unwrap_or_default()
            .chars()
            .collect();
        why.truncate(MOST_REASON);
        let line = loop {
            let line = json!({ "refused": why.iter().collect::<String>() }).to_string();
            if line.len() <= MOST_REASON || why.pop().is_none() {
                break line;
            }
        };
        format!("Burrow-Refused: {line}\r\n")
    };
    write_answer(output, &a, &header)
}

fn write_answer(output: &mut dyn Write, a: &Answer, headers: &str) -> io::Result<()> {
    let body = format!("{}\n", a.body);
    let reason = match a.status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        405 => "Method Not Allowed",
        411 => "Length Required",
        422 => "Unprocessable Content",
        503 => "Service Unavailable",
        _ => "Internal Server Error",
    };
    let answered = write!(
        output,
        "HTTP/1.0 {} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{headers}Connection: close\r\n\r\n{body}",
        a.status,
        body.len()
    )
    .and_then(|()| output.flush());
    match answered {
        // The client left before its answer (nginx reloaded or restarted
        // mid-upload). An upload is already in the store or refused, and
        // there's no one left to tell: that's not a failed door.
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        other => other,
    }
}

/// The head of a request: its method, and the headers the door reads.
#[derive(Debug, Default)]
struct Head {
    method: String,
    length: Option<u64>,
    token: Option<String>,
}

fn head(input: &mut dyn BufRead) -> Result<Head, String> {
    let mut lines = input.take(MOST_HEAD);
    let mut line = String::new();
    let mut head = Head::default();
    let mut first = true;
    loop {
        line.clear();
        if lines.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
            return Err("the request ended in its head".into());
        }
        let l = line.trim_end_matches(['\r', '\n']);
        if first {
            head.method = l.split(' ').next().unwrap_or_default().to_owned();
            first = false;
            continue;
        }
        if l.is_empty() {
            return Ok(head);
        }
        let Some((name, value)) = l.split_once(':') else {
            return Err("a header isn't NAME: VALUE".into());
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            head.length = Some(value.parse().map_err(|_| "Content-Length isn't a number")?);
        } else if name.eq_ignore_ascii_case("authorization") {
            head.token = value
                .strip_prefix("Bearer ")
                .map(|t| t.trim().to_owned())
                .filter(|t| !t.is_empty());
        }
    }
}

fn handle(cfg: &Config, input: &mut dyn BufRead) -> Answer {
    let head = match head(input) {
        Ok(h) => h,
        Err(e) => return refused(400, e),
    };
    if head.method != "POST" {
        return refused(405, "the door takes a POST of an upload");
    }
    let Some(length) = head.length else {
        return refused(411, "an upload says its Content-Length");
    };
    let mut body = input.take(length);
    let a = upload(cfg, head.token.as_deref(), &mut body);
    // nginx sends the whole body before it reads an answer: what a refusal
    // left unread is read and let go.
    let _ = io::copy(&mut body, &mut io::sink());
    a
}

/// The header-time check: the head's token, by [`admit`], with the audience
/// it claims. That holds it to everything but the manifest it names (who
/// signed it, until when, which job, which repo); the upload holds it to
/// that once nginx has the body.
fn check(cfg: &Config, input: &mut dyn BufRead) -> Answer {
    let head = match head(input) {
        Ok(h) => h,
        Err(e) => return refused(400, e),
    };
    let Some(token) = head.token else {
        return refused(401, NO_TOKEN);
    };
    let Some(claimed) = claimed_audience(&token) else {
        return refused(
            403,
            "the token isn't a job's for this door: it names no upload's audience",
        );
    };
    match admit(cfg, &token, &claimed) {
        Ok(()) => answer(200, json!({ "admitted": true })),
        Err(a) => a,
    }
}

/// The audience a token claims, unverified: its payload's `aud`, when that
/// is one upload's audience ([`is_audience`]) and nothing else. Only ever
/// handed to cn, which verifies the token signs it.
fn claimed_audience(token: &str) -> Option<String> {
    let [_, payload, _] = token.split('.').collect::<Vec<_>>()[..] else {
        return None;
    };
    let claims: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).ok()?).ok()?;
    claims["aud"]
        .as_str()
        .filter(|a| is_audience(a))
        .map(str::to_owned)
}

/// An upload's body: its manifest's line, then its NARs.
fn upload(cfg: &Config, token: Option<&str>, body: &mut dyn BufRead) -> Answer {
    let mut line = Vec::new();
    if let Err(e) = (&mut *body)
        .take(MOST_MANIFEST)
        .read_until(b'\n', &mut line)
    {
        return refused(400, format!("the body doesn't read: {e}"));
    }
    let Some(manifest) = line.strip_suffix(b"\n") else {
        return refused(400, "the body doesn't start with its manifest's line");
    };
    let parsed = match Manifest::parse(manifest) {
        Ok(m) => m,
        Err(e) => return refused(400, e),
    };
    let Some(token) = token else {
        return refused(401, NO_TOKEN);
    };
    if let Err(a) = admit(cfg, token, &audience(manifest)) {
        return a;
    }
    let key = match Key::open(&cfg.key) {
        Ok(k) => k,
        Err(e) => return refused(500, e),
    };
    let store = Store {
        dir: cfg.dir.clone(),
        max_size: cfg.max_size,
    };
    match store.upload(&parsed, body, &key) {
        Ok(done) => answer(200, json!(done)),
        Err(e @ Stopped::Mismatch { .. }) => refused(422, e.to_string()),
        Err(e @ Stopped::Short { .. }) => refused(400, e.to_string()),
        Err(e @ Stopped::Io(_)) => refused(500, e.to_string()),
    }
}

/// Whether the token's job may write the store: Conary's rule, asked of
/// `cn` with the repos the machine lists; then a public repo's alone, since
/// what the store holds is public to read, and a private repo's runs share
/// the machine's own store already.
fn admit(cfg: &Config, token: &str, audience: &str) -> Result<(), Answer> {
    let (program, before) = cfg
        .cn
        .split_first()
        .ok_or_else(|| refused(500, "no cn is configured"))?;
    let mut cn = Command::new(program);
    cn.args(before)
        .args(["attest", "job", "--audience", audience])
        .args(cfg.repos.iter().flat_map(|r| ["--repo", r.as_str()]))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cn
        .spawn()
        .map_err(|e| refused(500, format!("cn doesn't start: {e}")))?;
    // The token on stdin, never an argument any process could read.
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(token.as_bytes());
    }
    let out = child
        .wait_with_output()
        .map_err(|e| refused(500, format!("cn didn't finish: {e}")))?;
    let said = String::from_utf8_lossy(&out.stderr).trim().to_owned();
    match out.status.code() {
        Some(0) => {}
        Some(65) => return Err(refused(403, said)),
        Some(75) => return Err(refused(503, "GitHub can't be asked just now; try again")),
        _ => return Err(refused(500, format!("cn failed: {said}"))),
    }
    let job: Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| refused(500, format!("cn's answer doesn't read: {e}")))?;
    let repo = job["repository"].as_str().unwrap_or_default();
    match job["visibility"].as_str() {
        Some("public") => Ok(()),
        other => Err(refused(
            403,
            format!(
                "{repo} is {}: what the store holds is public, and a private repo's runs share the machine's own store",
                other.unwrap_or("of no visibility GitHub says")
            ),
        )),
    }
}

#[cfg(test)]
mod tests;
