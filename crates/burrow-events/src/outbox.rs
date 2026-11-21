//! conaryd's outbox (#1370): each delivery about a repo conaryd lands is
//! written here before GitHub is answered, and one forwarder thread sends
//! it on until conaryd takes it.
//!
//! conaryd's intake is serial (its receiver's lock, then a queue change
//! that saves its state, then its deliveries file), so its answer can take
//! longer than GitHub waits for ours, about ten seconds; and while it
//! restarts it takes nothing, which GitHub never sends again by itself. So
//! the request thread only holds the delivery, and GitHub hears 202 once
//! it's on disk (502 only when it couldn't be written). The forwarder sends
//! the oldest first, waits as long as conaryd takes to answer, and keeps a
//! delivery until conaryd answers 2xx. Sending one again is safe: conaryd
//! answers 202 for a delivery id it already holds, for its
//! DELIVERY_LIFETIME (a day). One still not taken near the end of that is
//! given up, with one `not taken by conaryd` line, so a real failure is
//! seen.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::http::Request;
use crate::landing::Landing;
use crate::now;

/// The headers conaryd reads: its signature check is over the same body
/// with the same secret, so the delivery goes on as GitHub signed it.
const FORWARDED: [&str; 4] = [
    "X-GitHub-Event",
    "X-GitHub-Delivery",
    "X-Hub-Signature-256",
    "Content-Type",
];

/// How long a send waits for conaryd's answer. It answers once its serial
/// intake has queued and saved the delivery: seconds, more behind a burst.
/// Two minutes without an answer is conaryd stuck, and the send is tried
/// again.
const SEND_TIMEOUT: Duration = Duration::from_secs(120);

/// How long conaryd on this machine has to accept the connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How old a held delivery gets before it's given up: an hour short of
/// conaryd's DELIVERY_LIFETIME (86400s), past which a resend would be a new
/// delivery to it rather than one it already has.
const GIVE_UP: f64 = 82_800.0;

/// The first wait after a failed send, doubled each time it fails again,
/// up to the longest.
const FIRST_RETRY: Duration = Duration::from_secs(1);
const LONGEST_RETRY: Duration = Duration::from_secs(60);

/// What a held delivery's file starts with, on a line of its own; the body
/// follows as GitHub sent it.
#[derive(Debug, Serialize, Deserialize)]
struct Held {
    repo: String,
    /// When it came in, in seconds since the epoch.
    received: f64,
    /// The FORWARDED headers it came with.
    headers: Vec<(String, String)>,
}

impl Held {
    fn header(&self, name: &str) -> &str {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map_or("?", |(_, v)| v.as_str())
    }
}

/// Why a send didn't take.
#[derive(Debug)]
enum Missed {
    /// conaryd answered, not 2xx: this delivery waits, the rest go on.
    Answered(u16),
    /// No answer (refused, timed out): none goes until conaryd answers.
    Unreached(String),
}

impl std::fmt::Display for Missed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Missed::Answered(code) => write!(f, "answered {code}"),
            Missed::Unreached(e) => f.write_str(e),
        }
    }
}

/// A delivery's next try, after it failed.
struct Retry {
    wait: Duration,
    at: Instant,
    why: String,
}

impl Retry {
    fn after(last: Option<&Retry>, why: String) -> Retry {
        let wait = last.map_or(FIRST_RETRY, |r| (r.wait * 2).min(LONGEST_RETRY));
        Retry {
            wait,
            at: Instant::now() + wait,
            why,
        }
    }
}

pub struct Outbox {
    landing: Landing,
    port: u16,
    dir: PathBuf,
    pub(crate) timeout: Duration,
    pub(crate) give_up: f64,
    count: AtomicU64,
    /// The forwarder's rounds so far: one a send, retry or new delivery.
    pub(crate) rounds: AtomicU64,
    /// Deliveries held so far, which the forwarder waits on.
    held: Mutex<u64>,
    wake: Condvar,
}

impl Outbox {
    /// The outbox the config's `landing` asks for: none when nothing goes
    /// to conaryd, and an error when something would but there's nowhere
    /// to hold it. Its directory is made if need be, and a write a crash
    /// cut short (a dot file) cleared.
    pub fn open(landing: &Landing) -> Result<Option<Outbox>, String> {
        let Some(port) = landing.forward else {
            return Ok(None);
        };
        let dir = landing
            .outbox
            .clone()
            .ok_or("landing: forward needs an outbox to hold deliveries in")?;
        fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        for e in fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))? {
            let e = e.map_err(|e| format!("{}: {e}", dir.display()))?;
            if e.file_name().to_string_lossy().starts_with('.') {
                let _ = fs::remove_file(e.path());
            }
        }
        Ok(Some(Outbox {
            landing: landing.clone(),
            port,
            dir,
            timeout: SEND_TIMEOUT,
            give_up: GIVE_UP,
            count: AtomicU64::new(0),
            rounds: AtomicU64::new(0),
            held: Mutex::new(0),
            wake: Condvar::new(),
        }))
    }

    /// Hold `req`, a signed delivery about `repo`, for conaryd if the
    /// landing record gives it the repo now. Ok(false): it doesn't. Ok(true)
    /// once the delivery is on disk, for the forwarder to send.
    pub fn hold(&self, repo: &str, req: &Request) -> io::Result<bool> {
        if self.landing.forward_to(repo).is_none() {
            return Ok(false);
        }
        let head = Held {
            repo: repo.into(),
            received: now(),
            headers: FORWARDED
                .iter()
                .filter_map(|&n| req.header(n).map(|v| (n.to_owned(), v.to_owned())))
                .collect(),
        };
        // Named by when it came, so the oldest goes first, across restarts
        // and request threads; never by anything the sender wrote.
        let name = format!(
            "{:020}-{:08}",
            (head.received * 1e6) as u64,
            self.count.fetch_add(1, Ordering::Relaxed)
        );
        let part = self.dir.join(format!(".{name}"));
        let write = || -> io::Result<()> {
            let mut f = File::create(&part)?;
            serde_json::to_writer(&mut f, &head)?;
            f.write_all(b"\n")?;
            f.write_all(&req.body)?;
            f.sync_all()?;
            fs::rename(&part, self.dir.join(&name))?;
            File::open(&self.dir)?.sync_all()
        };
        if let Err(e) = write() {
            let _ = fs::remove_file(&part);
            return Err(e);
        }
        *self.held.lock().unwrap_or_else(|p| p.into_inner()) += 1;
        self.wake.notify_all();
        Ok(true)
    }

    /// The held deliveries' names, oldest first.
    fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(&self.dir)
            .map(|d| {
                d.filter_map(Result::ok)
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| !n.starts_with('.'))
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    fn read(&self, name: &str) -> Result<(Held, Vec<u8>), String> {
        let mut bytes = Vec::new();
        File::open(self.dir.join(name))
            .and_then(|mut f| f.read_to_end(&mut bytes))
            .map_err(|e| e.to_string())?;
        let nl = bytes
            .iter()
            .position(|&b| b == b'\n')
            .ok_or("no header line")?;
        let held = serde_json::from_slice(&bytes[..nl]).map_err(|e| e.to_string())?;
        Ok((held, bytes.split_off(nl + 1)))
    }

    fn drop_held(&self, name: &str) {
        if let Err(e) = fs::remove_file(self.dir.join(name)) {
            eprintln!("conaryd's outbox: {name} sent but not cleared: {e}");
        }
    }

    /// Send what's held to conaryd, for as long as the service runs: the
    /// oldest first, each until conaryd answers 2xx. With no answer at all,
    /// nothing goes until a retry finds conaryd again; a delivery conaryd
    /// answers otherwise waits on its own, and the rest go on.
    pub fn forward(&self) {
        let mut retries: BTreeMap<String, Retry> = BTreeMap::new();
        // conaryd not answering: when to try again, and why.
        let mut down: Option<Retry> = None;
        loop {
            self.rounds.fetch_add(1, Ordering::Relaxed);
            let seen = *self.held.lock().unwrap_or_else(|p| p.into_inner());
            let names = self.names();
            retries.retain(|n, _| names.binary_search(n).is_ok());
            for name in &names {
                if down.as_ref().is_some_and(|d| d.at > Instant::now()) {
                    break;
                }
                if retries.get(name).is_some_and(|r| r.at > Instant::now()) {
                    continue;
                }
                let (held, body) = match self.read(name) {
                    Ok(it) => it,
                    Err(e) => {
                        // Dropped before it's said, so whoever reads the
                        // line finds it gone.
                        self.drop_held(name);
                        eprintln!("conaryd's outbox: {name} unreadable, dropped: {e}");
                        continue;
                    }
                };
                if now() - held.received > self.give_up {
                    let why = retries
                        .get(name)
                        .or(down.as_ref())
                        .map_or("never sent", |r| r.why.as_str());
                    // Dropped before it's said, as above.
                    self.drop_held(name);
                    eprintln!(
                        "{}: delivery {} ({}) not taken by conaryd: {why}",
                        held.repo,
                        held.header("X-GitHub-Delivery"),
                        held.header("X-GitHub-Event"),
                    );
                    continue;
                }
                match send(self.port, &held.headers, &body, self.timeout) {
                    Ok(()) => {
                        self.drop_held(name);
                        if down.take().is_some() {
                            eprintln!("conaryd answers again; sending what it missed");
                        }
                    }
                    Err(Missed::Answered(code)) => {
                        // conaryd answers, so the rest go on.
                        down = None;
                        // Said once, so a refusal that lasts (a secret
                        // that isn't conaryd's) shows before it's given up.
                        if !retries.contains_key(name) {
                            eprintln!(
                                "{}: delivery {} ({}): conaryd answered {code}; sending it again",
                                held.repo,
                                held.header("X-GitHub-Delivery"),
                                held.header("X-GitHub-Event"),
                            );
                        }
                        let retry = Retry::after(retries.get(name), format!("answered {code}"));
                        retries.insert(name.clone(), retry);
                    }
                    Err(Missed::Unreached(e)) => {
                        if down.is_none() {
                            eprintln!(
                                "conaryd on 127.0.0.1:{} not answering ({e}); holding deliveries until it does",
                                self.port
                            );
                        }
                        down = Some(Retry::after(down.as_ref(), e));
                    }
                }
            }
            let now_i = Instant::now();
            if down.as_ref().is_some_and(|d| d.at <= now_i) {
                // Nothing tried conaryd again this round (nothing held
                // was due): the next delivery finds out afresh.
                down = None;
            }
            // conaryd not answering: until the retry, whatever comes in.
            // Otherwise until a delivery waiting on its own is due, or a
            // new one comes.
            if let Some(d) = &down {
                std::thread::sleep(d.at.saturating_duration_since(now_i));
                continue;
            }
            let held = self.held.lock().unwrap_or_else(|p| p.into_inner());
            match retries.values().map(|r| r.at).min() {
                Some(at) => {
                    let wait = at.saturating_duration_since(Instant::now());
                    drop(self.wake.wait_timeout_while(held, wait, |n| *n == seen));
                }
                None => {
                    drop(self.wake.wait_while(held, |n| *n == seen));
                }
            }
        }
    }
}

/// POST a held delivery to conaryd on `port`, its body and headers as they
/// came. Ok once conaryd answered 2xx.
fn send(
    port: u16,
    headers: &[(String, String)],
    body: &[u8],
    timeout: Duration,
) -> Result<(), Missed> {
    let unreached = |e: io::Error| Missed::Unreached(e.to_string());
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut s = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).map_err(unreached)?;
    s.set_read_timeout(Some(timeout))
        .and_then(|()| s.set_write_timeout(Some(timeout)))
        .map_err(unreached)?;
    let mut head = format!(
        "POST / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (name, v) in headers {
        head.push_str(&format!("{name}: {v}\r\n"));
    }
    head.push_str("\r\n");
    s.write_all(head.as_bytes())
        .and_then(|()| s.write_all(body))
        .and_then(|()| s.flush())
        .map_err(unreached)?;
    // The status line is all that's wanted; a body, if any, is left.
    let mut got = Vec::new();
    let mut buf = [0u8; 256];
    while !got.windows(2).any(|w| w == b"\r\n") && got.len() < 4096 {
        match s.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => got.extend_from_slice(&buf[..n]),
            Err(e) => return Err(unreached(e)),
        }
    }
    let line = String::from_utf8_lossy(&got);
    let code: u16 = line
        .split(' ')
        .nth(1)
        .and_then(|c| c.trim().parse().ok())
        .ok_or_else(|| Missed::Unreached("no answer".into()))?;
    if (200..300).contains(&code) {
        Ok(())
    } else {
        Err(Missed::Answered(code))
    }
}
