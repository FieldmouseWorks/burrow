//! A stand-in for Healthchecks: takes pings on a local port, keeps each
//! one's path and body, and answers from a list, then by whether the path is
//! its one check's.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

pub struct Healthchecks {
    pub url: String,
    pings: Arc<Mutex<Vec<(String, String)>>>,
}

impl Healthchecks {
    /// Answers the first pings from `answers` in turn, then 200 for a path
    /// at or under `check` and 404 for any other.
    pub fn serve(check: &str, answers: &[u16]) -> Healthchecks {
        Healthchecks::serve_moved(check, answers, None)
    }

    /// The same, and every answer says `Location: moved_to`.
    pub fn serve_moved(check: &str, answers: &[u16], moved_to: Option<&str>) -> Healthchecks {
        let location = moved_to.map_or(String::new(), |to| format!("Location: {to}\r\n"));
        let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
        let url = format!("http://{}", listener.local_addr().expect("its address"));
        let pings = Arc::new(Mutex::new(Vec::new()));
        let answers = Arc::new(Mutex::new(answers.iter().copied().collect::<VecDeque<_>>()));
        let (kept, check) = (Arc::clone(&pings), check.to_owned());
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let Some((path, body)) = read(&stream) else {
                    continue;
                };
                let code = answers.lock().unwrap().pop_front().unwrap_or(
                    if path == check || path.starts_with(&format!("{check}/")) {
                        200
                    } else {
                        404
                    },
                );
                kept.lock().unwrap().push((path, body));
                let mut stream = stream;
                let _ = write!(
                    stream,
                    "HTTP/1.1 {code} X\r\n{location}Content-Length: 2\r\nConnection: close\r\n\r\nOK"
                );
            }
        });
        Healthchecks { url, pings }
    }

    pub fn pings(&self) -> Vec<(String, String)> {
        self.pings.lock().unwrap().clone()
    }

    pub fn last(&self) -> (String, String) {
        self.pings().pop().expect("a ping")
    }
}

/// A URL where nothing listens: a port the system just gave out and took back.
pub fn nobody() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
    format!("http://{}", listener.local_addr().expect("its address"))
}

/// A request's path and body.
fn read(stream: &TcpStream) -> Option<(String, String)> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let path = line.split_whitespace().nth(1)?.to_owned();
    let mut length = 0;
    loop {
        line.clear();
        reader.read_line(&mut line).ok()?;
        if line == "\r\n" || line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().ok()?;
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).ok()?;
    Some((path, String::from_utf8_lossy(&body).into_owned()))
}
