//! Closes the signed-in browser the way its own window would:
//! `Browser.close` over the DevTools protocol, then a wait for its main
//! process to exit. A SIGTERM ends Chromium without writing the cookies it
//! has held for under half a minute, so a login made just before a restart
//! or a reboot would be lost.

use std::fs::File;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::Value;

/// How long DevTools gets to connect, and for each read or write.
const STEP: Duration = Duration::from_secs(5);

/// How long the browser gets to exit once it's told to.
pub const EXIT_WITHIN: Duration = Duration::from_secs(20);

/// How often its main process is looked for meanwhile.
const LOOK_EVERY: Duration = Duration::from_millis(100);

#[derive(Debug, PartialEq, Eq)]
pub enum Closed {
    /// Nothing answered as DevTools on the port: there's nothing to close
    /// gracefully.
    NotAnswering,
    /// It took `Browser.close`, and its main process exited.
    Exited,
}

/// Tells the browser whose DevTools listen on `port` to close, then waits
/// up to `within` for its main process `pid` to be gone.
pub fn close(port: u16, pid: u32, within: Duration) -> Result<Closed, String> {
    let Some(url) = debugger_url(port)? else {
        return Ok(Closed::NotAnswering);
    };
    let path = url
        .split_once(&format!(":{port}"))
        .map(|(_, path)| path)
        .ok_or_else(|| format!("DevTools' WebSocket, {url}, isn't on port {port}"))?;
    let mut ws = connect(port).map_err(|e| format!("couldn't open the WebSocket: {e}"))?;
    handshake(&mut ws, port, path)?;
    let message = serde_json::json!({"id": 1, "method": "Browser.close"}).to_string();
    ws.write_all(&frame(message.as_bytes(), random()?))
        .map_err(|e| format!("couldn't send Browser.close: {e}"))?;
    let main = Path::new("/proc").join(pid.to_string());
    let deadline = Instant::now() + within;
    while main.exists() {
        if Instant::now() >= deadline {
            return Err(format!(
                "the browser was still up {within:?} after Browser.close"
            ));
        }
        thread::sleep(LOOK_EVERY);
    }
    Ok(Closed::Exited)
}

fn connect(port: u16) -> io::Result<TcpStream> {
    let to = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let stream = TcpStream::connect_timeout(&to, STEP)?;
    stream.set_read_timeout(Some(STEP))?;
    stream.set_write_timeout(Some(STEP))?;
    Ok(stream)
}

/// The browser's WebSocket, from `/json/version`; none when nothing answers
/// there as DevTools does (no connection, no answer, not a 200).
fn debugger_url(port: u16) -> Result<Option<String>, String> {
    let Ok(Some(body)) = version(port) else {
        return Ok(None);
    };
    let answer: Value = serde_json::from_slice(&body)
        .map_err(|e| format!("DevTools' /json/version isn't JSON: {e}"))?;
    answer["webSocketDebuggerUrl"]
        .as_str()
        .map(|url| Some(url.to_owned()))
        .ok_or_else(|| "DevTools' /json/version names no webSocketDebuggerUrl".to_owned())
}

/// `/json/version`'s body, if its answer is a 200. Read to its length, not
/// to the end: DevTools may keep the connection open.
fn version(port: u16) -> io::Result<Option<Vec<u8>>> {
    let mut s = connect(port)?;
    write!(
        s,
        "GET /json/version HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    )?;
    let (head, mut body) = read_head(&mut s)?;
    let head = String::from_utf8_lossy(&head);
    if !head.starts_with("HTTP/1.") || head.split(' ').nth(1) != Some("200") {
        return Ok(None);
    }
    let length = head.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("content-length")
            .then(|| value.trim().parse::<usize>().ok())?
    });
    match length {
        Some(length) => {
            let mut chunk = [0; 4096];
            while body.len() < length {
                match s.read(&mut chunk)? {
                    0 => break,
                    n => body.extend_from_slice(&chunk[..n]),
                }
            }
            body.truncate(length);
        }
        None => {
            s.read_to_end(&mut body)?;
        }
    }
    Ok(Some(body))
}

/// An answer's head, up to the blank line, and what came after it; the
/// whole of it if the connection ended first.
fn read_head(s: &mut TcpStream) -> io::Result<(Vec<u8>, Vec<u8>)> {
    let mut answer = Vec::new();
    let mut chunk = [0; 4096];
    loop {
        if let Some(end) = find(&answer, b"\r\n\r\n") {
            let rest = answer.split_off(end + 4);
            answer.truncate(end);
            return Ok((answer, rest));
        }
        match s.read(&mut chunk)? {
            0 => return Ok((answer, Vec::new())),
            n => answer.extend_from_slice(&chunk[..n]),
        }
    }
}

/// Upgrades the connection to a WebSocket, as a client does.
fn handshake(ws: &mut TcpStream, port: u16, path: &str) -> Result<(), String> {
    let key = base64::engine::general_purpose::STANDARD.encode(random::<16>()?);
    write!(
        ws,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\n\
         Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
    )
    .map_err(|e| format!("couldn't open the WebSocket: {e}"))?;
    let (head, _) =
        read_head(ws).map_err(|e| format!("DevTools didn't answer the WebSocket: {e}"))?;
    if !head.starts_with(b"HTTP/1.1 101") {
        let first = head
            .split(|&b| b == b'\r' || b == b'\n')
            .next()
            .unwrap_or(&[]);
        return Err(format!(
            "CDP refused the WebSocket: {}",
            String::from_utf8_lossy(first)
        ));
    }
    Ok(())
}

/// One text frame, masked with `mask`, as a client must send it.
pub fn frame(payload: &[u8], mask: [u8; 4]) -> Vec<u8> {
    let mut frame = vec![0x81];
    match payload.len() {
        n @ 0..=125 => frame.push(0x80 | n as u8),
        n @ 126..=0xffff => {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            frame.push(0x80 | 127);
            frame.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    frame.extend_from_slice(&mask);
    frame.extend(payload.iter().zip(mask.iter().cycle()).map(|(b, m)| b ^ m));
    frame
}

fn random<const N: usize>() -> Result<[u8; N], String> {
    let mut bytes = [0; N];
    File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|e| format!("/dev/urandom: {e}"))?;
    Ok(bytes)
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::frame;

    #[test]
    fn frames_a_message_masked() {
        let f = frame(b"hi", [1, 2, 3, 4]);
        assert_eq!(f, [0x81, 0x82, 1, 2, 3, 4, b'h' ^ 1, b'i' ^ 2]);
        let long = vec![b'x'; 300];
        let f = frame(&long, [0; 4]);
        assert_eq!(&f[..4], [0x81, 0x80 | 126, 1, 44]);
        assert_eq!(f.len(), 4 + 4 + 300);
        let huge = vec![b'x'; 70_000];
        let f = frame(&huge, [0; 4]);
        assert_eq!(&f[..2], [0x81, 0x80 | 127]);
        assert_eq!(u64::from_be_bytes(f[2..10].try_into().unwrap()), 70_000);
    }
}
