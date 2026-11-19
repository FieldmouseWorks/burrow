//! What a run built, to the machine's build store (`burrow.builds`,
//! crates/burrow-builds; docs/gate-design.md, "One build store, written by
//! whoever may sign"), as flake-check.yml's builds-write step has it after
//! the gate ran, passed or failed. What goes is what keep would keep
//! ([`picked`]): the paths this run's own store holds that no one signed,
//! which on a hosted runner's store, empty when the job started, is what
//! this run built or took from the kept builds.
//!
//! The door decides who may write, by the job's token, and this doesn't
//! try to: a job it refuses (a PR's run of its own copy of the workflow,
//! which can't sign either) has its builds left out, and says so. What the
//! store holds already (its narinfo answers) isn't sent again. Uploads
//! go in batches of at most [`BATCH`] bytes of NARs, each with a token of
//! its own: nginx takes a whole upload before the door reads its token,
//! which lives five minutes. Each batch lists a path after what it refers
//! to, so a store that stops part way holds no path without what it needs
//! from this run.

use std::collections::{BTreeSet, VecDeque};
use std::io::{self, Cursor, Read};
use std::process::{Child, ChildStdout, Command, Stdio};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use burrow_builds::{Entry, Manifest, audience, nix32};
use serde_json::Value;

use crate::{Store, picked};

/// The most bytes of NARs one upload carries, so that one sent at a few
/// MB/s still reaches the door inside its token's five minutes.
pub const BATCH: u64 = 512 << 20;

/// How many times a batch the door can't take just now (503) is sent again.
const AGAIN: usize = 2;

/// A path's NAR, as `nix-store --dump` writes it.
pub trait Nars {
    fn nar(&self, path: &str) -> Result<Box<dyn Read>, String>;
}

/// The store's door, and this job's tokens for it.
pub trait Door {
    /// Whether the store holds `path` already: its narinfo is there.
    fn holds(&self, path: &str) -> Result<bool, String>;
    /// This job's OIDC token, its audience `audience`.
    fn token(&self, audience: &str) -> Result<String, String>;
    /// POSTs `body`, `len` bytes, with `token`: the status and what the
    /// door said, or why it couldn't be asked.
    fn post(&self, token: &str, len: u64, body: &mut dyn Read) -> Result<(u16, String), String>;
}

/// What an upload did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Sent {
    /// Paths the store took, and those it held already.
    pub written: usize,
    pub skipped: usize,
    /// The NARs' bytes, and how many uploads carried them.
    pub bytes: u64,
    pub batches: usize,
    /// Why the door wouldn't take this run's builds, when it wouldn't.
    pub refused: Option<String>,
}

/// Uploads what `store` holds that [`picked`] picks, and the store doesn't,
/// through `door`.
pub fn upload(store: &Store, nars: &dyn Nars, door: &dyn Door) -> Result<Sent, String> {
    upload_in(store, nars, door, BATCH)
}

/// [`upload`], in batches of at most `batch` bytes of NARs.
fn upload_in(store: &Store, nars: &dyn Nars, door: &dyn Door, batch: u64) -> Result<Sent, String> {
    let mut sent = Sent::default();
    // What the store holds already isn't sent again: every leg of every
    // run would otherwise send it all.
    let mut entries = Vec::new();
    for p in ordered(store, &picked(store)) {
        if door.holds(&p)? {
            sent.skipped += 1;
        } else {
            entries.push(entry(store, &p)?);
        }
    }
    for batch in batches(entries, batch) {
        let manifest = Manifest { paths: batch };
        let mut line = serde_json::to_vec(&manifest).map_err(|e| e.to_string())?;
        let aud = audience(&line);
        line.push(b'\n');
        let len = line.len() as u64 + manifest.paths.iter().map(|e| e.nar_size).sum::<u64>();
        let mut tries = 0;
        let (status, said) = loop {
            let token = door.token(&aud)?;
            let mut body = Body {
                head: Cursor::new(line.clone()),
                rest: manifest.paths.iter().map(|e| e.path.clone()).collect(),
                now: None,
                nars,
            };
            let (status, said) = door.post(&token, len, &mut body)?;
            if status == 503 && tries < AGAIN {
                tries += 1;
                continue;
            }
            break (status, said);
        };
        match status {
            200 => {
                let done: Value = serde_json::from_str(&said)
                    .map_err(|e| format!("the store's answer doesn't read: {e}: {said}"))?;
                let count = |k: &str| done[k].as_array().map_or(0, Vec::len);
                sent.written += count("written");
                sent.skipped += count("skipped");
                sent.bytes += manifest.paths.iter().map(|e| e.nar_size).sum::<u64>();
                sent.batches += 1;
            }
            // The door's own decision: this job may not write. Every batch
            // would be refused alike.
            403 => {
                sent.refused = Some(refusal(&said));
                return Ok(sent);
            }
            _ => {
                return Err(format!(
                    "the store answered HTTP {status}: {}",
                    refusal(&said)
                ));
            }
        }
    }
    Ok(sent)
}

/// What the door's refusal says, or the answer as it is.
fn refusal(said: &str) -> String {
    serde_json::from_str::<Value>(said)
        .ok()
        .and_then(|v| v["refused"].as_str().map(str::to_owned))
        .unwrap_or_else(|| said.trim().to_owned())
}

/// `paths`, each after every path of them it refers to.
fn ordered(store: &Store, paths: &[String]) -> Vec<String> {
    let set: BTreeSet<&String> = paths.iter().collect();
    let mut done: BTreeSet<&String> = BTreeSet::new();
    let mut out = Vec::new();
    for p in paths {
        // Depth first, by hand: a closure can be deep.
        let mut stack = vec![(p, false)];
        while let Some((q, refs_done)) = stack.pop() {
            if done.contains(q) {
                continue;
            }
            if refs_done {
                done.insert(q);
                out.push(q.clone());
                continue;
            }
            stack.push((q, true));
            for r in &store.paths[q].references {
                if r != q && set.contains(r) && !done.contains(r) {
                    stack.push((r, false));
                }
            }
        }
    }
    out
}

/// One path as the door's manifest has it.
fn entry(store: &Store, path: &str) -> Result<Entry, String> {
    let i = &store.paths[path];
    Ok(Entry {
        path: path.to_owned(),
        nar_hash: nix_hash(&i.nar_hash).map_err(|e| format!("{path}: {e}"))?,
        nar_size: i.nar_size,
        references: i.references.clone(),
        deriver: None,
    })
}

/// A NAR's SHA-256 as the door reads it, `sha256:` and Nix's base-32, from
/// nix's own forms of it: SRI (`sha256-BASE64`, what path-info's JSON says)
/// or that one already.
pub fn nix_hash(hash: &str) -> Result<String, String> {
    if let Some(b64) = hash.strip_prefix("sha256-") {
        let bytes = STANDARD
            .decode(b64)
            .map_err(|_| format!("{hash:?} isn't a SHA-256"))?;
        if bytes.len() == 32 {
            return Ok(format!("sha256:{}", nix32(&bytes)));
        }
    } else if hash.strip_prefix("sha256:").is_some_and(|d| d.len() == 52) {
        return Ok(hash.to_owned());
    }
    Err(format!("{hash:?} isn't a SHA-256 nix writes"))
}

/// Entries in their order, in runs of at most `most` bytes of NARs (a path
/// bigger than that, alone).
fn batches(entries: Vec<Entry>, most: u64) -> Vec<Vec<Entry>> {
    let mut out: Vec<Vec<Entry>> = Vec::new();
    let mut size = 0;
    for e in entries {
        match out.last_mut() {
            Some(b) if size + e.nar_size <= most => {
                size += e.nar_size;
                b.push(e);
            }
            _ => {
                size = e.nar_size;
                out.push(vec![e]);
            }
        }
    }
    out
}

/// An upload's body: its manifest's line, then each path's NAR, read as
/// it's sent.
struct Body<'a> {
    head: Cursor<Vec<u8>>,
    rest: VecDeque<String>,
    now: Option<Box<dyn Read>>,
    nars: &'a dyn Nars,
}

impl Read for Body<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.head.read(buf)?;
        if n > 0 {
            return Ok(n);
        }
        loop {
            if let Some(r) = &mut self.now {
                let n = r.read(buf)?;
                if n > 0 {
                    return Ok(n);
                }
                self.now = None;
            }
            let Some(p) = self.rest.pop_front() else {
                return Ok(0);
            };
            self.now = Some(self.nars.nar(&p).map_err(io::Error::other)?);
        }
    }
}

/// `nix-store --dump`, on the store nix's config names.
pub struct Dump;

impl Nars for Dump {
    fn nar(&self, path: &str) -> Result<Box<dyn Read>, String> {
        let mut child = Command::new("nix-store")
            .args(["--dump", path])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("nix-store --dump {path}: {e}"))?;
        let out = child.stdout.take().expect("a piped stdout");
        Ok(Box::new(Dumped { child, out }))
    }
}

/// A dump's output, which fails at its end if the dump did.
struct Dumped {
    child: Child,
    out: ChildStdout,
}

impl Read for Dumped {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.out.read(buf)?;
        if n == 0 && !buf.is_empty() {
            let status = self.child.wait()?;
            if !status.success() {
                return Err(io::Error::other(format!("nix-store --dump: {status}")));
            }
        }
        Ok(n)
    }
}

/// The store at a URL, and this job's tokens from Actions.
pub struct Http {
    /// The store's URL, a substituter's: its door is URL/upload.
    pub url: String,
    /// Actions' token service: ACTIONS_ID_TOKEN_REQUEST_URL and _TOKEN.
    pub token_url: String,
    pub token_bearer: String,
    agent: ureq::Agent,
}

impl Http {
    /// The store at `url`: https, or http on this machine itself (a test's).
    pub fn new(url: &str, token_url: &str, token_bearer: &str) -> Result<Self, String> {
        let url = url.trim_end_matches('/');
        let local = ["http://127.0.0.1:", "http://localhost:", "http://[::1]:"];
        if !url.starts_with("https://") && !local.iter().any(|l| url.starts_with(l)) {
            return Err(format!(
                "the build store's URL is {url:?}: it takes https, as a job's token goes there"
            ));
        }
        let agent = ureq::Agent::config_builder()
            .timeout_connect(Some(std::time::Duration::from_secs(30)))
            // Inside the token's five minutes, which the door reads after
            // nginx has the whole upload.
            .timeout_global(Some(std::time::Duration::from_secs(270)))
            .http_status_as_error(false)
            .build()
            .into();
        Ok(Self {
            url: url.to_owned(),
            token_url: token_url.to_owned(),
            token_bearer: token_bearer.to_owned(),
            agent,
        })
    }
}

impl Door for Http {
    fn holds(&self, path: &str) -> Result<bool, String> {
        let hash = burrow_builds::hash_part(path)?;
        let r = self
            .agent
            .head(&format!("{}/{hash}.narinfo", self.url))
            .call()
            .map_err(|e| format!("the store at {} didn't answer: {e}", self.url))?;
        match r.status().as_u16() {
            200 => Ok(true),
            404 => Ok(false),
            s => Err(format!("the store answered HTTP {s} for {path}'s narinfo")),
        }
    }

    fn token(&self, audience: &str) -> Result<String, String> {
        let mut last = String::new();
        for _ in 0..3 {
            let got = self
                .agent
                .get(&format!("{}&audience={audience}", self.token_url))
                .header("Authorization", &format!("bearer {}", self.token_bearer))
                .call();
            match got {
                Ok(mut r) if r.status() == 200 => {
                    let v: Value = r
                        .body_mut()
                        .read_json()
                        .map_err(|e| format!("Actions' token doesn't read: {e}"))?;
                    return v["value"]
                        .as_str()
                        .filter(|t| !t.is_empty() && !t.contains(char::is_whitespace))
                        .map(str::to_owned)
                        .ok_or_else(|| "Actions gave no token".to_owned());
                }
                Ok(r) => last = format!("HTTP {}", r.status()),
                Err(e) => last = e.to_string(),
            }
        }
        Err(format!("no token from Actions: {last}"))
    }

    fn post(&self, token: &str, len: u64, body: &mut dyn Read) -> Result<(u16, String), String> {
        let mut r = self
            .agent
            .post(&format!("{}/upload", self.url))
            .header("Authorization", &format!("Bearer {token}"))
            .header("Content-Type", "application/octet-stream")
            .header("Content-Length", &len.to_string())
            .send(ureq::SendBody::from_reader(body))
            .map_err(|e| format!("the store at {} didn't answer: {e}", self.url))?;
        let status = r.status().as_u16();
        let said = r
            .body_mut()
            .with_config()
            .limit(1 << 20)
            .read_to_string()
            .unwrap_or_default();
        Ok((status, said))
    }
}

/// What [`upload`] said, as a line of the step's summary.
pub fn summary(sent: &Sent) -> String {
    match &sent.refused {
        Some(why) => format!("build store: this run's builds weren't taken: {why}"),
        None => format!(
            "build store: wrote {} paths, held {} already, {} MiB in {} uploads",
            sent.written,
            sent.skipped,
            sent.bytes.div_ceil(1 << 20),
            sent.batches
        ),
    }
}

#[cfg(test)]
mod tests;
