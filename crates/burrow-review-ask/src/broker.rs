//! The owner's door's broker and its client (modules/autoland/review-ask.nix).
//! The broker runs as the machine user, whose gh login the door acts as,
//! and listens on a Unix socket anyone may reach: its peer check is the
//! gate, so only the users it's given (the steward's, burrow.user, and the
//! flow's controller where it has one) may ask. Every asker goes through
//! it, the steward and conaryd's flow alike, so the door runs one way: as
//! the broker's child, with none of the asker's environment (its App token,
//! its credentials), and the login never leaves the broker's user.
//!
//! One request a connection: one line of JSON ([`Request`]), and nothing
//! more, so the connection's end is the asker gone (Conary's flow killed it
//! past its deadline), and the door is killed with it, process group and
//! all: a door the asker gave up on never posts. The answer is one line
//! ([`Reply`]): what the door printed, its one JSON line, and its exit,
//! which the client prints and exits with as the door would. What the door
//! says on stderr stays in the broker's journal.
//!
//! One door a head at a time: a second ask of a head whose door is still
//! running is refused, as the head's unit once was, and the door's own lock
//! keeps two heads' asks from reading and writing its record at once.
use crate::Outcome;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// How long a door may run: inside Conary's flow's 180s deadline for a door
/// (its flow/ask.rs), with the client's own wait between the two.
pub const DOOR: Duration = Duration::from_secs(150);
/// How long the client waits for the broker's answer: past the door's own
/// limit, inside the flow's deadline.
pub const WAIT: Duration = Duration::from_secs(170);
/// The most of a request, or of what the door prints, that's read.
const MOST: u64 = 64 << 10;

/// What the machine's flake gives the broker (review-ask.nix).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Where it listens.
    pub socket: PathBuf,
    /// Who may ask, by user name.
    pub askers: Vec<String>,
    /// The door's command, which the broker runs with REPO PR HEAD.
    pub door: Vec<String>,
}

/// One ask, as the client sends it.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub repo: String,
    pub pr: u64,
    pub head: String,
}

/// The broker's answer: what the door printed and its exit, or the
/// broker's own failure in the door's words.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reply {
    pub code: i32,
    pub said: String,
}

impl Reply {
    fn failed(why: impl Into<String>) -> Self {
        let out = Outcome::Failed { why: why.into() };
        Self {
            code: out.code(),
            said: serde_json::to_string(&out).unwrap_or_default(),
        }
    }
}

pub struct Broker {
    /// The uids that may ask.
    pub askers: Vec<u32>,
    pub door: Vec<String>,
    /// How long a door may run ([`DOOR`]).
    pub limit: Duration,
    /// The heads whose doors are running, by [`key`].
    live: Mutex<HashSet<String>>,
}

/// A head's key: a repo's name is GitHub's in any case.
fn key(r: &Request) -> String {
    format!("{}#{}@{}", r.repo.to_ascii_lowercase(), r.pr, r.head)
}

/// The uid of the user `name`, as /etc/passwd has it.
pub fn uid(passwd: &str, name: &str) -> Result<u32, String> {
    passwd
        .lines()
        .map(|l| l.split(':').collect::<Vec<_>>())
        .find(|f| f.first() == Some(&name))
        .and_then(|f| f.get(2)?.parse().ok())
        .ok_or_else(|| format!("no user {name:?}"))
}

/// Listen at `path`, in place of any socket a stopped broker left there.
/// Anyone may connect: the peer check is the gate, so its mode lets every
/// asker, each another user, reach it whatever the umask.
pub fn bind(path: &Path) -> io::Result<UnixListener> {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_socket() => std::fs::remove_file(path)?,
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} is there and isn't a socket", path.display()),
            ));
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o666))?;
    Ok(listener)
}

impl Broker {
    pub fn new(askers: Vec<u32>, door: Vec<String>) -> Self {
        Self {
            askers,
            door,
            limit: DOOR,
            live: Mutex::default(),
        }
    }

    /// Serve every connection, each on its own thread, until accepting
    /// fails for good.
    pub fn serve(self: Arc<Self>, listener: &UnixListener) -> io::Result<()> {
        use rustix::io::Errno;
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    let me = self.clone();
                    thread::spawn(move || me.connection(stream));
                }
                // Short of descriptors or memory, or a peer gone before it
                // was accepted: wait a moment.
                Err(e)
                    if [
                        Errno::MFILE,
                        Errno::NFILE,
                        Errno::NOBUFS,
                        Errno::NOMEM,
                        Errno::CONNABORTED,
                    ]
                    .iter()
                    .any(|n| e.raw_os_error() == Some(n.raw_os_error())) =>
                {
                    thread::sleep(Duration::from_millis(100));
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// One asker's connection, to its end.
    pub fn connection(&self, stream: UnixStream) {
        let reply = match self.answer(&stream) {
            Some(reply) => reply,
            // The asker went away: nothing to say.
            None => return,
        };
        let mut line = serde_json::to_vec(&reply).unwrap_or_default();
        line.push(b'\n');
        let _ = (&stream).write_all(&line);
        let _ = stream.shutdown(std::net::Shutdown::Both);
    }

    fn answer(&self, stream: &UnixStream) -> Option<Reply> {
        // The request is read before the peer is checked, so that one
        // refused reads why rather than a broken pipe; the client sends it
        // as soon as it connects.
        let mut read = BufReader::new(stream.try_clone().ok()?);
        let mut line = String::new();
        let got = stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .and_then(|()| (&mut read).take(MOST).read_line(&mut line))
            .and_then(|_| stream.set_read_timeout(None));
        match rustix::net::sockopt::socket_peercred(stream) {
            Ok(cred) if self.askers.contains(&cred.uid.as_raw()) => {}
            Ok(cred) => {
                return Some(Reply::failed(format!(
                    "the owner's door's broker: uid {} may not ask",
                    cred.uid.as_raw()
                )));
            }
            Err(e) => {
                return Some(Reply::failed(format!(
                    "the owner's door's broker: its peer can't be told: {e}"
                )));
            }
        }
        if let Err(e) = got {
            return Some(Reply::failed(format!("the owner's door's broker: {e}")));
        }
        if line.is_empty() {
            return None;
        }
        let request: Request = match serde_json::from_str(line.trim_end()) {
            Ok(r) => r,
            Err(e) => {
                return Some(Reply::failed(format!(
                    "the owner's door's broker: the request doesn't read: {e}"
                )));
            }
        };
        let key = key(&request);
        if !self.live.lock().ok()?.insert(key.clone()) {
            return Some(Reply::failed(format!(
                "the owner's door is already asking of {} #{} at {}",
                request.repo, request.pr, request.head
            )));
        }
        let reply = self.door(&request, read);
        if let Ok(mut live) = self.live.lock() {
            live.remove(&key);
        }
        reply
    }

    /// The door on `request`, killed if the asker goes first (anything that
    /// ends its read: it sends nothing after its request). None then.
    fn door(&self, request: &Request, mut read: BufReader<UnixStream>) -> Option<Reply> {
        let gone = Arc::new(AtomicBool::new(false));
        let watched = gone.clone();
        let watch = thread::spawn(move || {
            let mut byte = [0];
            loop {
                match read.read(&mut byte) {
                    Ok(1) => {}
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    _ => break,
                }
            }
            watched.store(true, Ordering::SeqCst);
        });
        let (program, args) = self.door.split_first()?;
        let mut command = Command::new(program);
        command
            .args(args)
            .args([&request.repo, &request.pr.to_string(), &request.head])
            // None of the broker's environment but where its user's gh
            // login is: the door's command sets the rest.
            .env_clear()
            .envs(std::env::var_os("HOME").map(|h| ("HOME", h)));
        let ran = burrow_process::output_unless(
            &mut command,
            None,
            Instant::now() + self.limit,
            MOST as usize,
            &|| gone.load(Ordering::SeqCst),
        );
        let abandoned = gone.load(Ordering::SeqCst);
        // The watch ends once the connection is shut down, after the reply.
        drop(watch);
        if abandoned {
            eprintln!(
                "burrow-review-ask: the asker of {} #{} at {} went away, and its door was stopped",
                request.repo, request.pr, request.head
            );
            return None;
        }
        Some(match ran {
            Ok(out) => {
                let err = String::from_utf8_lossy(&out.stderr);
                if !err.trim().is_empty() {
                    eprintln!(
                        "burrow-review-ask: {} #{}: {}",
                        request.repo,
                        request.pr,
                        err.trim()
                    );
                }
                let said = String::from_utf8_lossy(&out.stdout).trim().to_owned();
                match out.status.code() {
                    Some(code) => Reply { code, said },
                    None => Reply::failed(format!("the owner's door was killed: {}", out.status)),
                }
            }
            Err(why) => Reply::failed(format!("the owner's door: {why}")),
        })
    }
}

/// The broker, as `burrow-review-ask broker CONFIG` runs it.
pub fn main(args: &[String]) -> Result<(), String> {
    let [config] = args else {
        return Err("usage: burrow-review-ask broker CONFIG".into());
    };
    let cfg: Config = std::fs::read_to_string(config)
        .map_err(|e| format!("{config}: {e}"))
        .and_then(|s| serde_json::from_str(&s).map_err(|e| format!("{config}: {e}")))?;
    let passwd = std::fs::read_to_string("/etc/passwd").map_err(|e| format!("/etc/passwd: {e}"))?;
    let askers = cfg
        .askers
        .iter()
        .map(|n| uid(&passwd, n))
        .collect::<Result<_, _>>()?;
    let listener = bind(&cfg.socket).map_err(|e| format!("{}: {e}", cfg.socket.display()))?;
    Arc::new(Broker::new(askers, cfg.door))
        .serve(&listener)
        .map_err(|e| format!("{}: {e}", cfg.socket.display()))
}

/// Ask through the broker at `socket`: its reply, or the client's own
/// failure as one.
pub fn ask(socket: &Path, request: &Request, wait: Duration) -> Reply {
    let asked = (|| -> io::Result<Reply> {
        let stream = UnixStream::connect(socket)?;
        let mut line = serde_json::to_vec(request)?;
        line.push(b'\n');
        (&stream).write_all(&line)?;
        // Nothing more is sent, and the write side stays open: its end is
        // this asker gone.
        stream.set_read_timeout(Some(wait))?;
        let mut reply = String::new();
        BufReader::new(&stream)
            .take(MOST * 2)
            .read_line(&mut reply)?;
        if reply.is_empty() {
            return Err(io::Error::other("it closed the connection with no answer"));
        }
        Ok(serde_json::from_str(reply.trim_end())?)
    })();
    asked.unwrap_or_else(|e| {
        Reply::failed(format!(
            "the owner's door's broker at {}: {e}",
            socket.display()
        ))
    })
}

/// The client, as `burrow-review-ask ask SOCKET REPO PR HEAD` runs it: it
/// prints what the door said and exits as the door did.
pub fn client(args: &[String]) -> Reply {
    let [socket, repo, pr, head] = args else {
        return Reply::failed("usage: burrow-review-ask ask SOCKET REPO PR HEAD");
    };
    let Ok(pr) = pr.parse() else {
        return Reply::failed(format!("{pr:?} isn't a PR number"));
    };
    let request = Request {
        repo: repo.clone(),
        pr,
        head: head.clone(),
    };
    ask(Path::new(socket), &request, WAIT)
}

#[cfg(test)]
#[path = "broker_tests.rs"]
mod tests;
