//! A stand-in for Chromium's DevTools on a local port, and for the browser's
//! main process: `/json/version` names the browser's WebSocket, which takes
//! one frame and, as Chromium does on `Browser.close`, ends the browser.

#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::thread;

/// How the stand-in answers.
#[derive(Clone, Copy)]
pub enum Answer {
    /// As Chromium does: the browser ends on the frame.
    Closes,
    /// As `Closes`, with more on the connection after `/json/version`'s
    /// body than its length says.
    Trailing,
    /// It takes the frame and the browser stays up.
    StaysUp,
    /// `/json/version` answers with this status.
    Version(u16),
    /// `/json/version` answers 200 with this body (`{port}` is its port).
    Body(&'static str),
    /// It refuses the WebSocket.
    Refuses,
}

/// What reached it.
#[derive(Clone, Debug, Default)]
pub struct Seen {
    /// The request for `/json/version`.
    pub version: String,
    /// The request for the WebSocket.
    pub upgrade: String,
    /// The frame, as it came.
    pub frame: Vec<u8>,
}

pub struct DevTools {
    pub port: u16,
    seen: Arc<Mutex<Seen>>,
}

/// A stand-in for the browser's main process.
pub fn browser() -> Child {
    Command::new("sleep").arg("60").spawn().expect("sleep")
}

/// A port nothing listens on: one the system just gave out and took back.
pub fn nobody() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("a port")
        .local_addr()
        .expect("its address")
        .port()
}

impl DevTools {
    /// Answers as `answer` says; on the frame, ends `browser` if it closes.
    pub fn serve(answer: Answer, browser: Option<Child>) -> DevTools {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
        let port = listener.local_addr().expect("its address").port();
        let seen = Arc::new(Mutex::new(Seen::default()));
        let kept = Arc::clone(&seen);
        let mut browser = browser;
        thread::spawn(move || {
            // Kept open, as DevTools may keep them: what it reads ends where
            // the answer says, not where the connection does.
            let mut open = Vec::new();
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let head = read_head(&mut stream);
                if head.starts_with("GET /json/version ") {
                    kept.lock().unwrap().version = head;
                    let (status, body) = match answer {
                        Answer::Version(status) => (status, String::new()),
                        Answer::Body(body) => (200, body.replace("{port}", &port.to_string())),
                        _ => (
                            200,
                            format!(
                                r#"{{"Browser": "Chrome/140.0", "webSocketDebuggerUrl": "ws://127.0.0.1:{port}/devtools/browser/0b4e"}}"#
                            ),
                        ),
                    };
                    // In one write, so the extra comes with the body.
                    let extra = if let Answer::Trailing = answer {
                        "and then some"
                    } else {
                        ""
                    };
                    let _ = write!(
                        stream,
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}{extra}",
                        body.len()
                    );
                    open.push(stream);
                    continue;
                }
                kept.lock().unwrap().upgrade = head;
                if let Answer::Refuses = answer {
                    let _ = write!(
                        stream,
                        "HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n"
                    );
                    continue;
                }
                let _ = write!(
                    stream,
                    "HTTP/1.1 101 WebSocket Protocol Handshake\r\nUpgrade: WebSocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: x\r\n\r\n"
                );
                kept.lock().unwrap().frame = read_frame(&mut stream);
                if let (Answer::Closes | Answer::Trailing, Some(b)) = (answer, browser.as_mut()) {
                    let _ = b.kill();
                    let _ = b.wait();
                }
            }
        });
        DevTools { port, seen }
    }

    pub fn seen(&self) -> Seen {
        self.seen.lock().unwrap().clone()
    }
}

fn read_head(stream: &mut TcpStream) -> String {
    let mut head = Vec::new();
    let mut byte = [0];
    while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).unwrap_or(0) == 1 {
        head.push(byte[0]);
    }
    String::from_utf8_lossy(&head).into_owned()
}

/// One client frame with a payload under 126 bytes, header and mask
/// included.
fn read_frame(stream: &mut TcpStream) -> Vec<u8> {
    let mut start = [0; 2];
    if stream.read_exact(&mut start).is_err() {
        return Vec::new();
    }
    let mut rest = vec![0; 4 + usize::from(start[1] & 0x7f)];
    if stream.read_exact(&mut rest).is_err() {
        return start.to_vec();
    }
    [start.to_vec(), rest].concat()
}

/// A frame's payload, unmasked.
pub fn payload(frame: &[u8]) -> Vec<u8> {
    let mask = &frame[2..6];
    frame[6..]
        .iter()
        .zip(mask.iter().cycle())
        .map(|(b, m)| b ^ m)
        .collect()
}
