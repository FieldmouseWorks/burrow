//! Just enough HTTP for the two local ports: a POST with a Content-Length
//! body, from nginx or curl on this machine, answered with a bare status and
//! the connection closed. Anything else is turned away.

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

/// GitHub's cap on a delivery.
pub const MAX_BODY: usize = 25 * 1024 * 1024;
/// A request line and headers longer than this are no request of ours.
const MAX_HEAD: usize = 64 * 1024;
/// A client that sends nothing for this long is dropped.
const TIMEOUT: Duration = Duration::from_secs(30);

pub struct Request {
    pub method: String,
    /// The path, query and all, as sent.
    pub target: String,
    headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    /// A header's value; names are case-blind.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Read one request, or the status to turn it away with (None: the client
/// went away or stalled, so there's no one to answer).
pub fn read(stream: &mut TcpStream) -> Result<Request, Option<u16>> {
    stream.set_read_timeout(Some(TIMEOUT)).map_err(|_| None)?;
    let mut buf = Vec::new();
    let mut chunk = [0; 8192];
    let end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        if buf.len() > MAX_HEAD {
            return Err(Some(431));
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return Err(None),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    };
    let head = std::str::from_utf8(&buf[..end]).map_err(|_| Some(400))?;
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or("").split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (first.next(), first.next(), first.next(), first.next())
    else {
        return Err(Some(400));
    };
    if !version.starts_with("HTTP/1.") || !target.starts_with('/') {
        return Err(Some(400));
    }
    let mut headers = Vec::new();
    for line in lines {
        let (k, v) = line.split_once(':').ok_or(Some(400))?;
        headers.push((k.trim().to_owned(), v.trim().to_owned()));
    }
    let mut req = Request {
        method: method.to_owned(),
        target: target.to_owned(),
        headers,
        body: Vec::new(),
    };
    if req.method != "POST" {
        return Err(Some(501));
    }
    // A body comes with its length, and only so: GitHub and curl both send one.
    if req.header("Transfer-Encoding").is_some() {
        return Err(Some(411));
    }
    let len = match req.header("Content-Length") {
        None => 0,
        Some(v) => v.parse::<usize>().map_err(|_| Some(400))?,
    };
    if len > MAX_BODY {
        return Err(Some(413));
    }
    if req
        .header("Expect")
        .is_some_and(|v| v.eq_ignore_ascii_case("100-continue"))
    {
        stream
            .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
            .map_err(|_| None)?;
    }
    let mut body = buf.split_off(end + 4);
    if body.len() < len {
        let more = len - body.len();
        let got = Read::by_ref(stream)
            .take(more as u64)
            .read_to_end(&mut body)
            .map_err(|_| None)?;
        if got < more {
            return Err(None);
        }
    }
    body.truncate(len);
    req.body = body;
    Ok(req)
}

pub fn answer(stream: &mut TcpStream, code: u16) {
    let reason = match code {
        202 => "Accepted",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        411 => "Length Required",
        413 => "Content Too Large",
        431 => "Request Header Fields Too Large",
        501 => "Not Implemented",
        _ => "",
    };
    // 204 may carry no Content-Length.
    let length = if code == 204 {
        ""
    } else {
        "Content-Length: 0\r\n"
    };
    let _ = write!(
        stream,
        "HTTP/1.1 {code} {reason}\r\n{length}Connection: close\r\n\r\n"
    );
    let _ = stream.flush();
}

/// After turning a request away mid-send: closing with its rest unread would
/// reset the connection, and the client could lose the answer. So say we're
/// done, and let it finish sending for a moment.
fn linger(stream: &mut TcpStream) {
    let _ = stream.shutdown(Shutdown::Write);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
    let _ = io::copy(&mut stream.take(MAX_BODY as u64), &mut io::sink());
}

/// Serve each request on 127.0.0.1:PORT (0: any free port) with `handle`, a
/// thread a connection. Returns the port it listens on.
pub fn listen(port: u16, handle: Arc<dyn Fn(&Request) -> u16 + Send + Sync>) -> io::Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    let port = listener.local_addr()?.port();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let handle = handle.clone();
            thread::spawn(move || match read(&mut stream) {
                Ok(req) => answer(&mut stream, handle(&req)),
                Err(Some(code)) => {
                    answer(&mut stream, code);
                    linger(&mut stream);
                }
                Err(None) => {}
            });
        }
    });
    Ok(port)
}

/// Whether GitHub signed this body with the secret (X-Hub-Signature-256).
pub fn signed(secret: &str, body: &[u8], header: Option<&str>) -> bool {
    let Some(hex) = header.and_then(|h| h.strip_prefix("sha256=")) else {
        return false;
    };
    let Some(sig) = unhex(hex) else {
        return false;
    };
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(body);
    // In constant time.
    mac.verify_slice(&sig).is_ok()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| s.get(i..i + 2).and_then(|b| u8::from_str_radix(b, 16).ok()))
        .collect()
}

#[cfg(test)]
pub mod tests {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::sync::Arc;
    use std::time::Duration;

    use hmac::{Hmac, KeyInit, Mac};
    use sha2::Sha256;

    use super::{listen, signed};

    pub fn sign(secret: &str, body: &[u8]) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        let hex: String = mac
            .finalize()
            .into_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        format!("sha256={hex}")
    }

    /// Send raw bytes, in pieces with pauses between if asked, and read the
    /// status back.
    pub fn send(port: u16, pieces: &[&[u8]]) -> u16 {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        for p in pieces {
            s.write_all(p).unwrap();
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        // The final answer, after any 100 Continue.
        let last = out
            .lines()
            .rev()
            .find(|l| l.starts_with("HTTP/1.1 "))
            .unwrap_or("");
        last.split(' ')
            .nth(1)
            .and_then(|c| c.parse().ok())
            .unwrap_or(0)
    }

    /// POST BODY to PATH with these headers.
    pub fn post(port: u16, path: &str, body: &[u8], headers: &[(&str, &str)]) -> u16 {
        let mut head = format!(
            "POST {path} HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n",
            body.len()
        );
        for (k, v) in headers {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        head.push_str("\r\n");
        send(port, &[head.as_bytes(), body])
    }

    #[test]
    fn signatures() {
        let body = br#"{"repository": {"full_name": "sam/r"}}"#;
        let sig = sign("hook-secret", body);
        assert!(signed("hook-secret", body, Some(&sig)));
        assert!(!signed("other", body, Some(&sig)));
        assert!(!signed("hook-secret", body, None));
        assert!(!signed("hook-secret", b"{}", Some(&sig)));
        assert!(!signed(
            "hook-secret",
            body,
            Some(&sig.replace("sha256=", "sha1="))
        ));
        assert!(!signed("hook-secret", body, Some(&format!("{sig}0"))));
        assert!(!signed("hook-secret", body, Some("sha256=zz")));
        assert!(!signed(
            "hook-secret",
            body,
            Some(&format!("{}é", &sig[..sig.len() - 2]))
        ));
    }

    // The body it reads is the one Content-Length says, however it arrives;
    // what isn't a POST with a length is turned away.
    #[test]
    fn requests() {
        let port = listen(
            0,
            Arc::new(|req| {
                if req.body == b"hello" && req.target == "/k?x=1" {
                    202
                } else {
                    400
                }
            }),
        )
        .unwrap();
        assert_eq!(post(port, "/k?x=1", b"hello", &[]), 202);
        assert_eq!(
            send(
                port,
                &[
                    b"POST /k?x=1 HTTP/1.1\r\nContent-Le",
                    b"ngth: 5\r\n\r",
                    b"\nhel",
                    b"lo"
                ]
            ),
            202
        );
        assert_eq!(
            post(port, "/k?x=1", b"hello", &[("Expect", "100-continue")]),
            202
        );
        assert_eq!(send(port, &[b"GET /k HTTP/1.1\r\n\r\n"]), 501);
        assert_eq!(send(port, &[b"POST /k HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n"]), 411);
        assert_eq!(
            send(port, &[b"POST /k HTTP/1.1\r\nContent-Length: x\r\n\r\n"]),
            400
        );
        assert_eq!(
            send(
                port,
                &[b"POST /k HTTP/1.1\r\nContent-Length: 26214401\r\n\r\n"]
            ),
            413
        );
        assert_eq!(send(port, &[b"POST k HTTP/1.1\r\n\r\n"]), 400);
        assert_eq!(send(port, &[b"nonsense\r\n\r\n"]), 400);
        assert_eq!(send(port, &[b"POST /k HTTP/1.1\r\nno colon\r\n\r\n"]), 400);
        // Just past the cap, so the server has read all of it when it answers.
        let start = "POST /k HTTP/1.1\r\nX: ";
        let long = format!("{start}{}", "a".repeat(64 * 1024 + 4 - start.len()));
        assert_eq!(send(port, &[long.as_bytes()]), 431);
    }
}
