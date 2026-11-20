//! Who a repo's landing events go to (Conary#313): the steward, through the
//! handlers marked `landing`, or conaryd, which gets each delivery as GitHub
//! sent it. The landing record (burrow_settings::landing) says which, and is
//! read again for every delivery, kick and pass, since burrow-landing moves
//! a repo from one to the other while this service runs.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use burrow_settings::landing::{Owner, Record};
use serde::Deserialize;

use crate::http::Request;

/// How long conaryd has to take a forwarded delivery: it answers once it's
/// queued, before any pass, so a few seconds is long.
const FORWARD_TIMEOUT: Duration = Duration::from_secs(5);

/// The headers conaryd reads: its signature check is over the same body
/// with the same secret, so the delivery goes on as GitHub signed it.
const FORWARDED: [&str; 4] = [
    "X-GitHub-Event",
    "X-GitHub-Delivery",
    "X-Hub-Signature-256",
    "Content-Type",
];

/// The config's `landing`: the record, and conaryd's webhook port on this
/// machine (none: nothing goes to conaryd).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Landing {
    pub record: PathBuf,
    #[serde(default)]
    pub forward: Option<u16>,
}

/// Whether the steward's handlers take `repo` now: while the record says
/// the steward holds it, a handover away included (its passes let go of
/// their work). A record that can't be read lets them through: the steward
/// reads it too, through its own decision, and lands nothing then.
pub fn steward_holds(record: &Path, repo: &str) -> bool {
    match Record::read(record) {
        Ok(r) => r.holding(repo).owner == Owner::Steward,
        Err(_) => true,
    }
}

impl Landing {
    /// conaryd's port, if `repo`'s deliveries go to it now: it holds the
    /// repo and no handover is under way. Never on a record that can't be
    /// read.
    pub fn forward_to(&self, repo: &str) -> Option<u16> {
        let port = self.forward?;
        let record = Record::read(&self.record).ok()?;
        record.holding(repo).lands(Owner::Conaryd).then_some(port)
    }
}

/// POST the delivery to conaryd on `port`, its body and the headers it
/// reads as they came. Ok once conaryd answered 2xx.
pub fn forward(port: u16, req: &Request) -> Result<(), String> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut s = TcpStream::connect_timeout(&addr, FORWARD_TIMEOUT).map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(FORWARD_TIMEOUT))
        .and_then(|()| s.set_write_timeout(Some(FORWARD_TIMEOUT)))
        .map_err(|e| e.to_string())?;
    let mut head = format!(
        "POST / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: {}\r\nConnection: close\r\n",
        req.body.len()
    );
    for name in FORWARDED {
        if let Some(v) = req.header(name) {
            head.push_str(&format!("{name}: {v}\r\n"));
        }
    }
    head.push_str("\r\n");
    s.write_all(head.as_bytes())
        .and_then(|()| s.write_all(&req.body))
        .and_then(|()| s.flush())
        .map_err(|e| e.to_string())?;
    // The status line is all that's wanted; a body, if any, is left.
    let mut got = Vec::new();
    let mut buf = [0u8; 256];
    while !got.windows(2).any(|w| w == b"\r\n") && got.len() < 4096 {
        match s.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => got.extend_from_slice(&buf[..n]),
            Err(e) => return Err(e.to_string()),
        }
    }
    let line = String::from_utf8_lossy(&got);
    let code: u16 = line
        .split(' ')
        .nth(1)
        .and_then(|c| c.trim().parse().ok())
        .ok_or("no answer")?;
    if (200..300).contains(&code) {
        Ok(())
    } else {
        Err(format!("answered {code}"))
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeSet;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use std::thread;

    use burrow_settings::landing::{Holding, Owner, Record};
    use serde_json::json;

    use super::Landing;
    use crate::handler::{Config, Handler};
    use crate::http::listen;
    use crate::http::tests::{post, sign};
    use crate::line::{Line, Pass};
    use crate::passes::{Units, Yields, serve};
    use crate::routes::{Handlers, Lines, delivery, kick};

    const ALONE: Yields<'static> = Yields {
        first: None,
        behind: Vec::new(),
    };

    /// The record saying `repo` is `owner`'s, `to` it if a handover is
    /// under way.
    fn hold(record: &Path, repo: &str, owner: Owner, to: Option<Owner>) {
        let mut r = Record::default();
        r.repos.insert(
            repo.into(),
            Holding {
                owner,
                to,
                generation: 1,
                since: 0,
            },
        );
        r.write(record).unwrap();
    }

    /// conaryd, stood in for: answers `code` and keeps each request whole.
    fn conaryd(code: u16) -> (u16, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let got = Arc::new(Mutex::new(Vec::new()));
        let kept = got.clone();
        thread::spawn(move || {
            for s in listener.incoming() {
                let mut s = s.unwrap();
                s.set_read_timeout(Some(std::time::Duration::from_millis(300)))
                    .unwrap();
                let mut req = Vec::new();
                let mut buf = [0u8; 4096];
                while let Ok(n) = s.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    req.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&req);
                    if let Some((head, body)) = text.split_once("\r\n\r\n") {
                        let len: usize = head
                            .lines()
                            .find_map(|l| l.strip_prefix("Content-Length: "))
                            .and_then(|n| n.parse().ok())
                            .unwrap_or(0);
                        if body.len() >= len {
                            break;
                        }
                    }
                }
                kept.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&req).into());
                let _ = write!(s, "HTTP/1.1 {code} X\r\nContent-Length: 0\r\n\r\n");
            }
        });
        (port, got)
    }

    struct Ports {
        lines: Lines,
        handlers: Handlers,
        hooks: u16,
        kicks: u16,
    }

    /// The steward's handler (`landing`) and the inbox's beside it, their
    /// config as modules/events.nix writes it, with this record and
    /// forward port.
    fn setup(record: &Path, forward: Option<u16>) -> Ports {
        let cfg: Config = serde_json::from_value(json!({
            "port": null,
            "kickPort": 0,
            "landing": {"record": record, "forward": forward},
            "handlers": {
                "autoland": {"events": ["pull_request", "push"], "unit": "autoland", "landing": true},
                "inbox": {"events": ["pull_request"], "unit": "proj-inbox"},
            },
        }))
        .unwrap();
        let cfg = cfg.wire();
        let landing = cfg.landing.clone();
        let handlers: Handlers = Arc::new(cfg.handlers);
        let lines: Lines = Arc::new(
            handlers
                .keys()
                .map(|k| (k.clone(), Line::new(0.0)))
                .collect(),
        );
        let (hs, ls) = (handlers.clone(), lines.clone());
        let (hooks, _) = listen(
            0,
            Arc::new(move |req| delivery(&hs, &ls, landing.as_ref(), crate::test_secret(), req)),
        )
        .unwrap();
        let (hs, ls) = (handlers.clone(), lines.clone());
        let (kicks, _) = listen(0, Arc::new(move |req| kick(&hs, &ls, req))).unwrap();
        Ports {
            lines,
            handlers,
            hooks,
            kicks,
        }
    }

    const BODY: &[u8] = br#"{"repository": {"full_name": "Sam/R"}, "action": "opened"}"#;

    fn deliver(port: u16) -> u16 {
        post(
            port,
            "/burrow/hook",
            BODY,
            &[
                ("X-Hub-Signature-256", &sign(crate::test_secret(), BODY)),
                ("X-GitHub-Event", "pull_request"),
                ("X-GitHub-Delivery", "d-1"),
                ("Content-Type", "application/json"),
            ],
        )
    }

    fn due(lines: &Lines, name: &str) -> BTreeSet<Pass> {
        let due = lines[name].due().into_keys();
        let issues = lines[name].issues().into_keys();
        due.chain(issues).collect()
    }

    fn just(repo: &str) -> BTreeSet<Pass> {
        BTreeSet::from([Pass::repo(repo)])
    }

    #[derive(Default)]
    struct Started(RefCell<Vec<String>>);
    impl Units for Started {
        fn state(&self, _: &str) -> String {
            "inactive".into()
        }
        fn start(&self, unit: &str) -> bool {
            self.0.borrow_mut().push(unit.into());
            true
        }
    }

    /// A wake-up for `repo`, due now.
    fn wake(p: &Ports, repo: &str) {
        let at = json!(crate::now());
        p.lines["autoland"].wakes(&serde_json::Map::from_iter([(repo.to_string(), at)]));
    }

    /// One round of the steward's line, the passes it started.
    fn run(p: &Ports) -> Vec<String> {
        let units = Started::default();
        let h: &Handler = &p.handlers["autoland"];
        serve(
            "autoland",
            h,
            &p.lines["autoland"],
            &ALONE,
            &units,
            true,
            || {},
        );
        units.0.into_inner()
    }

    // (a) The steward's handler takes a repo's deliveries, kicks and
    // wake-ups while the steward holds it, none once conaryd does, and
    // takes them again once it's back. Other handlers take it throughout.
    #[test]
    fn the_steward_hears_only_its_own_repos() {
        let dir = tempfile::tempdir().unwrap();
        let record = dir.path().join("owners.json");
        let p = setup(&record, None);
        // No record yet: every repo the steward's.
        assert_eq!(deliver(p.hooks), 202);
        assert_eq!(due(&p.lines, "autoland"), just("sam/r"));
        p.lines["autoland"].clear();
        p.lines["inbox"].clear();

        hold(&record, "sam/r", Owner::Conaryd, None);
        assert_eq!(deliver(p.hooks), 202);
        assert_eq!(due(&p.lines, "autoland"), BTreeSet::new());
        assert_eq!(due(&p.lines, "inbox"), just("sam/r"));
        assert_eq!(post(p.kicks, "/kick/autoland/sam/r", b"", &[]), 404);
        assert_eq!(post(p.kicks, "/kick/autoland", b"", &[]), 202);
        assert_eq!(due(&p.lines, "autoland"), BTreeSet::from([Pass::Full]));
        // A wake-up booked before the handover is spent, not started, and
        // the same booking read again isn't taken again.
        let early = serde_json::Map::from_iter([("sam/r".to_string(), json!(0.5))]);
        p.lines["autoland"].wakes(&early);
        assert_eq!(run(&p), vec!["autoland.service".to_string()]);
        p.lines["autoland"].wakes(&early);
        assert_eq!(due(&p.lines, "autoland"), BTreeSet::new());
        // Mid-handover back (conaryd letting go): still not the steward's.
        hold(&record, "sam/r", Owner::Conaryd, Some(Owner::Steward));
        assert_eq!(post(p.kicks, "/kick/autoland/sam/r", b"", &[]), 404);

        hold(&record, "sam/r", Owner::Steward, None);
        assert_eq!(post(p.kicks, "/kick/autoland/sam/r", b"", &[]), 202);
        assert_eq!(deliver(p.hooks), 202);
        assert_eq!(due(&p.lines, "autoland"), just("sam/r"));
        wake(&p, "sam/r");
        assert_eq!(run(&p), vec!["autoland@sam-r.service".to_string()]);
    }

    // (b) While the steward lets go (a handover to conaryd under way), its
    // passes go on, to vacate, and nothing goes to conaryd yet; (c) once
    // conaryd holds it, the delivery goes to conaryd whole, and a
    // steward-held repo's never does.
    #[test]
    fn conaryd_gets_its_repos_deliveries_whole() {
        let dir = tempfile::tempdir().unwrap();
        let record = dir.path().join("owners.json");
        let (port, got) = conaryd(202);
        let p = setup(&record, Some(port));

        assert_eq!(deliver(p.hooks), 202);
        hold(&record, "sam/r", Owner::Steward, Some(Owner::Conaryd));
        assert_eq!(deliver(p.hooks), 202);
        assert_eq!(due(&p.lines, "autoland"), just("sam/r"));
        wake(&p, "sam/r");
        assert_eq!(run(&p), vec!["autoland@sam-r.service".to_string()]);
        hold(&record, "sam/r", Owner::Conaryd, Some(Owner::Steward));
        assert_eq!(deliver(p.hooks), 202);
        assert!(got.lock().unwrap().is_empty());

        hold(&record, "sam/r", Owner::Conaryd, None);
        assert_eq!(deliver(p.hooks), 202);
        let got = got.lock().unwrap().clone();
        assert_eq!(got.len(), 1);
        let (head, body) = got[0].split_once("\r\n\r\n").unwrap();
        assert_eq!(body.as_bytes(), BODY);
        for h in [
            format!("X-Hub-Signature-256: {}", sign(crate::test_secret(), BODY)),
            "X-GitHub-Event: pull_request".into(),
            "X-GitHub-Delivery: d-1".into(),
            "Content-Type: application/json".into(),
        ] {
            assert!(head.lines().any(|l| l == h), "{h} in {head}");
        }
        assert!(head.starts_with("POST / HTTP/1.1\r\n"));
    }

    // (d) A delivery conaryd refuses, or that finds no conaryd, answers
    // GitHub 502.
    #[test]
    fn a_forward_not_taken_is_502() {
        let dir = tempfile::tempdir().unwrap();
        let record = dir.path().join("owners.json");
        hold(&record, "sam/r", Owner::Conaryd, None);
        let (port, got) = conaryd(500);
        assert_eq!(deliver(setup(&record, Some(port)).hooks), 502);
        assert_eq!(got.lock().unwrap().len(), 1);
        let closed = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        assert_eq!(deliver(setup(&record, Some(closed)).hooks), 502);
    }

    // (e) A record that can't be read lets the steward's handler through
    // (the steward refuses itself) and sends conaryd nothing.
    #[test]
    fn an_unreadable_record_forwards_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let record = dir.path().join("owners.json");
        fs::write(&record, b"{not json").unwrap();
        let (port, got) = conaryd(202);
        let p = setup(&record, Some(port));
        assert_eq!(deliver(p.hooks), 202);
        assert_eq!(due(&p.lines, "autoland"), just("sam/r"));
        assert_eq!(post(p.kicks, "/kick/autoland/sam/r", b"", &[]), 202);
        assert!(got.lock().unwrap().is_empty());
        assert_eq!(
            Landing {
                record,
                forward: Some(port)
            }
            .forward_to("sam/r"),
            None
        );
    }

    // With no landing in the config, nothing changes: the steward's
    // handler takes every repo.
    #[test]
    fn no_landing_config_is_the_steward_everywhere() {
        let cfg: Config = serde_json::from_value(json!({
            "port": null, "kickPort": 0,
            "handlers": {"autoland": {"events": ["push"], "unit": "autoland", "landing": true}},
        }))
        .unwrap();
        let cfg = cfg.wire();
        assert!(cfg.landing.is_none());
        assert!(cfg.handlers["autoland"].holds("sam/r"));
    }
}
