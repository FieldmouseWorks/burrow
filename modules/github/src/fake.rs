//! For the tests: a stand-in for GitHub's REST API that answers each request
//! as `route` says and keeps what it was asked, and a throwaway App key.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;

/// A token in GitHub's stateless format (`ghs_APPID_` and a JWT): long, and
/// with dots, hyphens and underscores in it.
pub const TOKEN: &str = "ghs_5518199_eyJhbGciOiAiUlMyNTYiLCAidHlwIjogIkpXVCJ9.eyJpc3MiOiAiNTUxODE5OSIsICJzdWIiOiAiMTc2MzUxNjUxIiwgImF1ZCI6ICJnaXRodWIuY29tIiwgImlhdCI6IDE3OTMyMjQ4MDAsICJleHAiOiAxNzkzMjI4NDAwLCAicmVwb3NpdG9yaWVzIjogWyJzYW0vciJdLCAicGVybWlzc2lvbnMiOiB7ImNvbnRlbnRzIjogIndyaXRlIiwgInB1bGxfcmVxdWVzdHMiOiAid3JpdGUifSwgImp0aSI6ICJ4eHh4eHh4eCJ9.uCRNAomB1pOve0Vq-O-kytY9KC4Z_xSULCRuUNk1HSJwSoAqccNYC2Nw3kzrKTwySoQjNCVX1OXDhDjw42kQ7ntUtmg2wfvdE9JEHZ4UNNxiymd_to9f5mpGS6rezb0AV2-Na1rDvMgIRLfVCxzGYDREu-fPz4_Aqh7jxjbZ4zn6uEjJtleoU-43wJy_3RSdCzgHsZHd6bYjzNlSgd0YcFtIyJsVA5A4RbuldTlFNR_mtFSFJ2D3NSnPAcqPadzK5Fv1gX3flKovekBwcfDu3GvrmPdotM0z0RdtRNFWOkWl1yEikOt2cMZ4axNZGu2shkeJk4leiyTmEgFKuqa6BA";

pub struct Request {
    pub method: String,
    pub path: String,
    pub auth: String,
    pub body: String,
}

/// An answer: status, extra headers, body.
pub type Answer = (u16, Vec<(&'static str, String)>, String);

pub struct Fake {
    pub url: String,
    pub asked: Arc<Mutex<Vec<Request>>>,
}

impl Fake {
    pub fn asked(&self) -> Vec<String> {
        self.asked
            .lock()
            .unwrap()
            .iter()
            .map(|r| format!("{} {}", r.method, r.path))
            .collect()
    }
}

pub fn serve(route: impl Fn(&Request) -> Answer + Send + 'static) -> Fake {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let asked = Arc::new(Mutex::new(Vec::new()));
    let kept = asked.clone();
    thread::spawn(move || {
        for s in listener.incoming() {
            let Ok(mut s) = s else { break };
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut line = String::new();
            r.read_line(&mut line).unwrap();
            let mut parts = line.split(' ');
            let (method, path) = (
                parts.next().unwrap().to_owned(),
                parts.next().unwrap().to_owned(),
            );
            let (mut len, mut auth) = (0, String::new());
            loop {
                let mut h = String::new();
                r.read_line(&mut h).unwrap();
                let h = h.trim_end();
                if h.is_empty() {
                    break;
                }
                let (k, v) = h.split_once(": ").unwrap();
                match k.to_lowercase().as_str() {
                    "content-length" => len = v.parse().unwrap(),
                    "authorization" => auth = v.to_owned(),
                    _ => {}
                }
            }
            let mut body = vec![0; len];
            r.read_exact(&mut body).unwrap();
            let request = Request {
                method,
                path,
                auth,
                body: String::from_utf8(body).unwrap(),
            };
            let (status, headers, body) = route(&request);
            kept.lock().unwrap().push(request);
            let mut out = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
                body.len()
            );
            for (k, v) in headers {
                out += &format!("{k}: {v}\r\n");
            }
            out += &format!("\r\n{body}");
            s.write_all(out.as_bytes()).unwrap();
        }
    });
    Fake { url, asked }
}

/// A new RSA key, PEM: PKCS#1 as GitHub gives one, or PKCS#8.
pub fn key(pkcs1: bool) -> Vec<u8> {
    let mut genrsa = Command::new("openssl");
    genrsa.arg("genrsa");
    if pkcs1 {
        genrsa.arg("-traditional");
    }
    let out = genrsa.arg("2048").output().expect("openssl, to make a key");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// The App's installations, a page of them: `(id, login)`.
pub fn installations(of: &[(u64, &str)]) -> String {
    let all: Vec<_> = of
        .iter()
        .map(|(id, login)| serde_json::json!({ "id": id, "account": { "login": login } }))
        .collect();
    serde_json::Value::from(all).to_string()
}
