//! A stand-in for Robot's webservice: takes calls on a local port, keeps
//! each one, and answers from its routes, or 404 as Robot does. A 3xx
//! answer sends the caller to `/elsewhere`.

#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

/// One call as it reached the stand-in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Asked {
    pub method: String,
    pub path: String,
    pub authorization: String,
    pub content_type: String,
    pub body: String,
}

pub struct Webservice {
    pub url: String,
    asked: Arc<Mutex<Vec<Asked>>>,
}

impl Webservice {
    /// Answers "METHOD /path" from `routes` (a status and a body), and
    /// anything else with Robot's 404.
    pub fn serve(routes: &[(&str, u16, &str)]) -> Webservice {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
        let url = format!("http://{}", listener.local_addr().expect("its address"));
        let asked = Arc::new(Mutex::new(Vec::new()));
        let routes: HashMap<String, (u16, String)> = routes
            .iter()
            .map(|(r, s, b)| ((*r).to_owned(), (*s, (*b).to_owned())))
            .collect();
        let kept = Arc::clone(&asked);
        let elsewhere = format!("{url}/elsewhere");
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let Some(call) = read(&stream) else {
                    continue;
                };
                let (status, body) = routes
                    .get(&format!("{} {}", call.method, call.path))
                    .cloned()
                    .unwrap_or((
                        404,
                        r#"{"error":{"status":404,"code":"NOT_FOUND","message":"Not found"}}"#
                            .to_owned(),
                    ));
                kept.lock().unwrap().push(call);
                let location = if (300..400).contains(&status) {
                    format!("Location: {elsewhere}\r\n")
                } else {
                    String::new()
                };
                let mut stream = stream;
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n{location}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        Webservice { url, asked }
    }

    pub fn asked(&self) -> Vec<Asked> {
        self.asked.lock().unwrap().clone()
    }

    /// Each call's method and path.
    pub fn calls(&self) -> Vec<String> {
        self.asked()
            .iter()
            .map(|a| format!("{} {}", a.method, a.path))
            .collect()
    }
}

/// A URL where nothing listens: a port the system just gave out and took back.
pub fn nobody() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
    format!("http://{}", listener.local_addr().expect("its address"))
}

fn read(stream: &TcpStream) -> Option<Asked> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut words = line.split_whitespace();
    let mut asked = Asked {
        method: words.next()?.to_owned(),
        path: words.next()?.to_owned(),
        ..Asked::default()
    };
    let mut length = 0;
    loop {
        line.clear();
        reader.read_line(&mut line).ok()?;
        if line == "\r\n" || line.is_empty() {
            break;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().to_owned();
        match name.to_ascii_lowercase().as_str() {
            "content-length" => length = value.parse().ok()?,
            "authorization" => asked.authorization = value,
            "content-type" => asked.content_type = value,
            _ => {}
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).ok()?;
    asked.body = String::from_utf8_lossy(&body).into_owned();
    Some(asked)
}
