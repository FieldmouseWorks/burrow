//! Who a repo's landing events go to (Conary#313): the steward, through the
//! handlers marked `landing`, or conaryd, which gets each delivery as GitHub
//! sent it, through its outbox (crate::outbox). The landing record (burrow_settings::landing) says which, and is
//! read again for every delivery, kick and pass, since burrow-landing moves
//! a repo from one to the other while this service runs.

use std::path::{Path, PathBuf};

use burrow_settings::landing::{Owner, Record};
use serde::Deserialize;

/// The config's `landing`: the record, conaryd's webhook port on this
/// machine (none: nothing goes to conaryd), and the directory its
/// deliveries are held in until it takes them (crate::outbox).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Landing {
    pub record: PathBuf,
    #[serde(default)]
    pub forward: Option<u16>,
    #[serde(default)]
    pub outbox: Option<PathBuf>,
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

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeSet;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use burrow_settings::landing::{Holding, Owner, Record};
    use serde_json::json;

    use super::Landing;
    use crate::handler::{Config, Handler};
    use crate::http::listen;
    use crate::http::tests::{post, sign};
    use crate::line::{Line, Pass};
    use crate::outbox::Outbox;
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

    /// conaryd, stood in for on `listener`: answers each request in turn
    /// with the next of `answers` (the last one ever after), after
    /// `delay`, and keeps each request whole.
    fn conaryd_on(
        listener: TcpListener,
        delay: Duration,
        answers: &[u16],
    ) -> Arc<Mutex<Vec<String>>> {
        let got = Arc::new(Mutex::new(Vec::new()));
        let kept = got.clone();
        let mut answers = answers.to_vec();
        thread::spawn(move || {
            for s in listener.incoming() {
                let mut s = s.unwrap();
                s.set_read_timeout(Some(Duration::from_millis(300)))
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
                thread::sleep(delay);
                let code = if answers.len() > 1 {
                    answers.remove(0)
                } else {
                    answers[0]
                };
                let _ = write!(s, "HTTP/1.1 {code} X\r\nContent-Length: 0\r\n\r\n");
            }
        });
        got
    }

    /// conaryd, stood in for: answers `code` at once.
    fn conaryd(code: u16) -> (u16, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        (port, conaryd_on(listener, Duration::ZERO, &[code]))
    }

    /// Whether `ok` comes true within ten seconds.
    fn soon(ok: impl Fn() -> bool) -> bool {
        let end = Instant::now() + Duration::from_secs(10);
        while Instant::now() < end {
            if ok() {
                return true;
            }
            thread::sleep(Duration::from_millis(20));
        }
        ok()
    }

    struct Ports {
        lines: Lines,
        handlers: Handlers,
        hooks: u16,
        kicks: u16,
        /// conaryd's outbox, and what sends it on.
        outbox: PathBuf,
        forwarder: Option<Arc<Outbox>>,
    }

    impl Ports {
        /// The deliveries held for conaryd.
        fn held(&self) -> usize {
            fs::read_dir(&self.outbox).map_or(0, Iterator::count)
        }
    }

    /// The steward's handler (`landing`) and the inbox's beside it, their
    /// config as modules/events.nix writes it, with this record and
    /// forward port, and the outbox's forwarder running.
    fn setup(record: &Path, forward: Option<u16>) -> Ports {
        setup_with(record, forward, |_| {})
    }

    /// The same, the outbox tuned first.
    fn setup_with(record: &Path, forward: Option<u16>, tune: impl FnOnce(&mut Outbox)) -> Ports {
        let outbox = record.with_file_name("outbox");
        let cfg: Config = serde_json::from_value(json!({
            "deliveries": false,
            "kickPort": 0,
            "landing": {"record": record, "forward": forward, "outbox": outbox},
            "handlers": {
                "autoland": {"events": ["pull_request", "push"], "unit": "autoland", "landing": true},
                "inbox": {"events": ["pull_request"], "unit": "proj-inbox"},
            },
        }))
        .unwrap();
        let cfg = cfg.wire();
        let held = cfg
            .landing
            .as_ref()
            .and_then(|l| Outbox::open(l).unwrap())
            .map(|mut o| {
                tune(&mut o);
                Arc::new(o)
            });
        let forwarder = held.clone();
        if let Some(o) = held.clone() {
            thread::spawn(move || o.forward());
        }
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
            Arc::new(move |req| delivery(&hs, &ls, held.as_deref(), crate::test_secret(), req)),
        )
        .unwrap();
        let (hs, ls) = (handlers.clone(), lines.clone());
        let (kicks, _) = listen(0, Arc::new(move |req| kick(&hs, &ls, req))).unwrap();
        Ports {
            lines,
            handlers,
            hooks,
            kicks,
            outbox,
            forwarder,
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
        // Nothing held, so nothing sent later either.
        assert_eq!(p.held(), 0);
        assert!(got.lock().unwrap().is_empty());

        hold(&record, "sam/r", Owner::Conaryd, None);
        assert_eq!(deliver(p.hooks), 202);
        assert!(soon(|| p.held() == 0 && !got.lock().unwrap().is_empty()));
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

    // (d) A conaryd slower to answer than GitHub waits for us gets the
    // delivery once, and GitHub hears 202 without waiting for it.
    #[test]
    fn a_slow_conaryd_gets_it_once() {
        let dir = tempfile::tempdir().unwrap();
        let record = dir.path().join("owners.json");
        hold(&record, "sam/r", Owner::Conaryd, None);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let got = conaryd_on(listener, Duration::from_secs(6), &[202]);
        let p = setup(&record, Some(port));
        let started = Instant::now();
        assert_eq!(deliver(p.hooks), 202);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(soon(|| p.held() == 0));
        assert!(started.elapsed() >= Duration::from_secs(6));
        assert_eq!(got.lock().unwrap().len(), 1);
    }

    // (e) A delivery conaryd refuses, or that finds no conaryd, is 202 and
    // held, and sent again until conaryd takes it: once it listens, and
    // once more after it refused.
    #[test]
    fn a_delivery_not_taken_is_sent_again() {
        let dir = tempfile::tempdir().unwrap();
        let record = dir.path().join("owners.json");
        hold(&record, "sam/r", Owner::Conaryd, None);
        let closed = burrow_test::ClosedPort::new();
        let p = setup(&record, Some(closed.port()));
        assert_eq!(deliver(p.hooks), 202);
        thread::sleep(Duration::from_millis(300));
        assert_eq!(p.held(), 1);
        let got = conaryd_on(closed.listen(), Duration::ZERO, &[500, 202]);
        assert!(soon(|| p.held() == 0));
        assert_eq!(got.lock().unwrap().len(), 2);
        let got = got.lock().unwrap().clone();
        assert_eq!(got[0], got[1]);
    }

    // (f) One that conaryd hasn't taken by the time it would forget the
    // delivery's id is given up, rather than sent again as a new one.
    #[test]
    fn a_delivery_too_old_is_given_up() {
        let dir = tempfile::tempdir().unwrap();
        let record = dir.path().join("owners.json");
        hold(&record, "sam/r", Owner::Conaryd, None);
        let closed = burrow_test::ClosedPort::new();
        let p = setup_with(&record, Some(closed.port()), |o| o.give_up = 0.5);
        assert_eq!(deliver(p.hooks), 202);
        assert_eq!(p.held(), 1);
        assert!(soon(|| p.held() == 0));
    }

    // (g) The forwarder rests between tries: after conaryd refused a
    // connection and then answered other than 2xx, and once what it held
    // is given up while conaryd is away.
    #[test]
    fn the_forwarder_waits_between_tries() {
        let rounds = |p: &Ports| {
            p.forwarder
                .as_ref()
                .unwrap()
                .rounds
                .load(std::sync::atomic::Ordering::Relaxed)
        };
        let dir = tempfile::tempdir().unwrap();
        let record = dir.path().join("owners.json");
        hold(&record, "sam/r", Owner::Conaryd, None);
        let closed = burrow_test::ClosedPort::new();
        let p = setup(&record, Some(closed.port()));
        assert_eq!(deliver(p.hooks), 202);
        thread::sleep(Duration::from_millis(300));
        let got = conaryd_on(closed.listen(), Duration::ZERO, &[500]);
        assert!(soon(|| !got.lock().unwrap().is_empty()));
        let before = rounds(&p);
        thread::sleep(Duration::from_millis(1500));
        assert!(rounds(&p) - before < 10, "{} rounds", rounds(&p) - before);

        let dir = tempfile::tempdir().unwrap();
        let record = dir.path().join("owners.json");
        hold(&record, "sam/r", Owner::Conaryd, None);
        let closed = burrow_test::ClosedPort::new();
        let p = setup_with(&record, Some(closed.port()), |o| o.give_up = 0.5);
        assert_eq!(deliver(p.hooks), 202);
        assert!(soon(|| p.held() == 0));
        let before = rounds(&p);
        thread::sleep(Duration::from_millis(1000));
        assert!(rounds(&p) - before < 3, "{} rounds", rounds(&p) - before);
    }

    // (h) A delivery for conaryd that can't be held is answered 502, so
    // GitHub's log shows it; one about a repo conaryd doesn't land is
    // still taken.
    #[test]
    fn a_delivery_not_held_is_502() {
        let dir = tempfile::tempdir().unwrap();
        let record = dir.path().join("owners.json");
        hold(&record, "sam/r", Owner::Conaryd, None);
        let (port, got) = conaryd(202);
        let p = setup(&record, Some(port));
        fs::remove_dir(&p.outbox).unwrap();
        fs::write(&p.outbox, b"").unwrap();
        assert_eq!(deliver(p.hooks), 502);
        hold(&record, "sam/r", Owner::Steward, None);
        assert_eq!(deliver(p.hooks), 202);
        assert!(got.lock().unwrap().is_empty());
    }

    // (i) A record that can't be read lets the steward's handler through
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
        assert_eq!(p.held(), 0);
        assert!(got.lock().unwrap().is_empty());
        assert_eq!(
            Landing {
                record,
                forward: Some(port),
                outbox: None,
            }
            .forward_to("sam/r"),
            None
        );
    }

    // Forwarding with nowhere to hold what's forwarded is no config.
    #[test]
    fn forward_needs_an_outbox() {
        let landing = |outbox| Landing {
            record: "/nonexistent".into(),
            forward: Some(1),
            outbox,
        };
        assert!(Outbox::open(&landing(None)).is_err());
        let dir = tempfile::tempdir().unwrap();
        assert!(
            Outbox::open(&landing(Some(dir.path().join("o"))))
                .unwrap()
                .is_some()
        );
        let none = Landing {
            forward: None,
            ..landing(None)
        };
        assert!(Outbox::open(&none).unwrap().is_none());
    }

    // With no landing in the config, nothing changes: the steward's
    // handler takes every repo.
    #[test]
    fn no_landing_config_is_the_steward_everywhere() {
        let cfg: Config = serde_json::from_value(json!({
            "deliveries": false, "kickPort": 0,
            "handlers": {"autoland": {"events": ["push"], "unit": "autoland", "landing": true}},
        }))
        .unwrap();
        let cfg = cfg.wire();
        assert!(cfg.landing.is_none());
        assert!(cfg.handlers["autoland"].holds("sam/r"));
    }
}
