//! A look's only way out of its sandbox, which has no network of its own.
//! `bridge`, the first thing run in there, listens on 127.0.0.1 and passes
//! each connection over a unix socket to the `Gate` the service runs outside
//! for that one look. The gate lets through only what the agent needs: a
//! CONNECT to one of its API's hosts, on 443 (the agent's HTTPS_PROXY).
//! Anything else gets a 403, so a command a PR talks an agent into running
//! reaches nothing: not the machine's own services, not the LAN, not the web.

use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream, ToSocketAddrs};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const FORBIDDEN: &[u8] =
    b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
const CONNECTED: &[u8] = b"HTTP/1.1 200 Connection established\r\n\r\n";
/// The most of a request's head the gate reads before it gives up on it.
const HEAD: usize = 65536;
/// How long the way to an API waits on it: to connect, and for each read and
/// write after.
const PATIENCE: Duration = Duration::from_secs(60);

/// How the gate reaches an API's host, on 443.
pub type Dial = Arc<dyn Fn(&str) -> io::Result<TcpStream> + Send + Sync>;

/// A TCP connection to host:443.
pub fn dial(host: &str) -> io::Result<TcpStream> {
    let mut last = io::Error::new(io::ErrorKind::NotFound, format!("{host}: no address"));
    for addr in (host, 443).to_socket_addrs()? {
        match TcpStream::connect_timeout(&addr, PATIENCE) {
            Ok(s) => {
                s.set_read_timeout(Some(PATIENCE))?;
                s.set_write_timeout(Some(PATIENCE))?;
                return Ok(s);
            }
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// One end of a connection the gate or the bridge passes along.
pub trait End: Read + Write + Send + Sized + 'static {
    fn again(&self) -> io::Result<Self>;
    fn no_more(&self);
}

impl End for TcpStream {
    fn again(&self) -> io::Result<Self> {
        self.try_clone()
    }
    fn no_more(&self) {
        let _ = self.shutdown(Shutdown::Write);
    }
}

impl End for UnixStream {
    fn again(&self) -> io::Result<Self> {
        self.try_clone()
    }
    fn no_more(&self) {
        let _ = self.shutdown(Shutdown::Write);
    }
}

/// a's bytes to b until a ends, then b hears there's no more.
fn pipe(mut a: impl Read, b: &mut impl End) {
    let mut buf = vec![0; 65536];
    loop {
        match a.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if b.write_all(&buf[..n]).is_err() {
                    break;
                }
            }
        }
    }
    b.no_more();
}

/// Both ways between a and b until both ends are done; `first` goes to b
/// ahead of the rest.
pub fn splice(a: impl End, mut b: impl End, first: &[u8]) -> io::Result<()> {
    if !first.is_empty() {
        b.write_all(first)?;
    }
    let (mut back_to, back_from) = (a.again()?, b.again()?);
    let back = thread::spawn(move || pipe(back_from, &mut back_to));
    pipe(a, &mut b);
    let _ = back.join();
    Ok(())
}

/// The gate for one look, listening on a unix socket, open until closed.
pub struct Gate {
    path: PathBuf,
    closed: Arc<AtomicBool>,
    serving: Option<JoinHandle<()>>,
}

impl Gate {
    /// The gate on the socket `path`, which lets through a CONNECT to one of
    /// `hosts` on 443 and nothing else.
    pub fn open(path: &Path, hosts: &[&str], dial: Dial) -> io::Result<Gate> {
        let listener = UnixListener::bind(path)?;
        let hosts: Arc<Vec<String>> =
            Arc::new(hosts.iter().map(|h| h.to_ascii_lowercase()).collect());
        let closed = Arc::new(AtomicBool::new(false));
        let done = closed.clone();
        let serving = thread::spawn(move || {
            for c in listener.incoming() {
                if done.load(Ordering::SeqCst) {
                    return;
                }
                let Ok(c) = c else { continue };
                let (hosts, dial) = (hosts.clone(), dial.clone());
                thread::spawn(move || one(c, &hosts, &dial));
            }
        });
        Ok(Gate {
            path: path.to_owned(),
            closed,
            serving: Some(serving),
        })
    }
}

impl Drop for Gate {
    /// No more connections: what's under way goes on until either end ends.
    fn drop(&mut self) {
        self.closed.store(true, Ordering::SeqCst);
        // The listener waits in accept: a connection of its own wakes it.
        let _ = UnixStream::connect(&self.path);
        if let Some(t) = self.serving.take() {
            let _ = t.join();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

/// What the gate makes of one request head: where it may go, if anywhere.
pub fn allowed<'a>(head: &'a [u8], hosts: &[String]) -> Option<&'a str> {
    let line = match head.windows(2).position(|w| w == b"\r\n") {
        Some(i) => &head[..i],
        None => head,
    };
    let line = std::str::from_utf8(line).ok()?;
    let mut words = line.split(' ');
    let (method, target) = (words.next()?, words.next()?);
    let (host, port) = target.rsplit_once(':')?;
    (method == "CONNECT" && port == "443" && hosts.contains(&host.to_ascii_lowercase()))
        .then_some(host)
}

fn one(mut c: UnixStream, hosts: &[String], dial: &Dial) {
    let mut head = Vec::new();
    let mut buf = vec![0; 65536];
    let end = loop {
        if let Some(i) = head.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        match c.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(_) if head.len() > HEAD => return,
            Ok(n) => head.extend_from_slice(&buf[..n]),
        }
    };
    let (head, rest) = (&head[..end], &head[end + 4..]);
    let Some(host) = allowed(head, hosts) else {
        let _ = c.write_all(FORBIDDEN);
        return;
    };
    let Ok(up) = dial(host) else { return };
    if c.write_all(CONNECTED).is_ok() {
        let _ = splice(c, up, rest);
    }
}

/// In the sandbox: listen on 127.0.0.1, hand each connection to the gate at
/// `path`, and run argv with each of `names` in its environment set to the
/// listener's URL. Its exit code is argv's.
pub fn bridge(path: &Path, names: &[&str], argv: &[OsString]) -> Result<i32, String> {
    let srv = TcpListener::bind("127.0.0.1:0").map_err(|e| format!("can't listen: {e}"))?;
    let url = format!(
        "http://127.0.0.1:{}",
        srv.local_addr().map_err(|e| e.to_string())?.port()
    );
    let gate = path.to_owned();
    thread::spawn(move || {
        for c in srv.incoming().flatten() {
            let gate = gate.clone();
            thread::spawn(move || {
                if let Ok(u) = UnixStream::connect(&gate) {
                    let _ = splice(c, u, b"");
                }
            });
        }
    });
    let (program, args) = argv.split_first().ok_or("nothing to run")?;
    let status = Command::new(program)
        .args(args)
        .envs(names.iter().map(|n| (n, &url)))
        .status()
        .map_err(|e| format!("{}: {e}", program.to_string_lossy()))?;
    Ok(status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)))
}

#[cfg(test)]
mod tests {
    use super::allowed;

    /// Only a CONNECT, only to one of its hosts in any case, only on 443.
    #[test]
    fn only_a_connect_to_an_apis_host_on_443() {
        fn at(head: &str) -> Option<&str> {
            allowed(
                head.as_bytes(),
                &["chatgpt.com".to_owned(), "api.openai.com".to_owned()],
            )
        }
        assert_eq!(at("CONNECT chatgpt.com:443 HTTP/1.1"), Some("chatgpt.com"));
        assert_eq!(
            at("CONNECT ChatGPT.com:443 HTTP/1.1\r\nHost: x"),
            Some("ChatGPT.com")
        );
        for no in [
            "CONNECT chatgpt.com:80 HTTP/1.1",
            "CONNECT example.com:443 HTTP/1.1",
            "CONNECT chatgpt.com HTTP/1.1",
            "CONNECT chatgpt.com:443\rX HTTP/1.1",
            "CONNECT chatgpt.com.evil:443 HTTP/1.1",
            "CONNECT evil.com#chatgpt.com:443 HTTP/1.1",
            "GET https://chatgpt.com:443/ HTTP/1.1",
            "POST /v1/messages HTTP/1.1",
            "connect chatgpt.com:443 HTTP/1.1",
            "CONNECT",
            "",
        ] {
            assert_eq!(at(no), None, "{no}");
        }
    }
}
