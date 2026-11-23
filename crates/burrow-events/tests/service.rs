//! The binary as the service runs it: its config, its credential, the
//! systemctl on its PATH.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use burrow_settings::landing::{Holding, Owner, Record};
use hmac::{Hmac, KeyInit, Mac};
use serde_json::{Value, json};
use sha2::Sha256;

struct Service {
    child: Child,
    lines: mpsc::Receiver<String>,
}

impl Service {
    fn start(dir: &Path, config: Value, credentials: &Path) -> Service {
        // As systemd hands it over: the socket is the holder's, non-blocking,
        // and the service's stdin.
        let socket = TcpListener::bind("127.0.0.1:0").unwrap();
        socket.set_nonblocking(true).unwrap();
        Service::start_on(dir, config, credentials, &socket)
    }

    fn start_on(dir: &Path, config: Value, credentials: &Path, socket: &TcpListener) -> Service {
        let path = dir.join("events.json");
        fs::write(&path, config.to_string()).unwrap();
        let mut child = Command::new(burrow_test::program!("burrow-events"))
            .env("EVENTS_CONFIG", &path)
            .env("CREDENTIALS_DIRECTORY", credentials)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    dir.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .stdin(Stdio::from(OwnedFd::from(socket.try_clone().unwrap())))
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let (tx, lines) = mpsc::channel();
        let stderr = child.stderr.take().unwrap();
        thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let _ = tx.send(line);
            }
        });
        Service { child, lines }
    }

    /// The first line of its log that starts so, waiting up to ten seconds.
    fn said(&self, start: &str) -> String {
        let end = Instant::now() + Duration::from_secs(10);
        while let Some(left) = end.checked_duration_since(Instant::now()) {
            match self.lines.recv_timeout(left) {
                Ok(line) if line.starts_with(start) => return line,
                Ok(_) => {}
                Err(_) => break,
            }
        }
        panic!("it never said {start:?}");
    }

    /// The port in a "... on 127.0.0.1:PORT..." line.
    fn port(&self, start: &str) -> u16 {
        let line = self.said(start);
        let rest = line.split("127.0.0.1:").nth(1).unwrap();
        rest.split(|c: char| !c.is_ascii_digit())
            .next()
            .unwrap()
            .parse()
            .unwrap()
    }

    /// Whether no line of its log for `wait` has `word` in it.
    fn never_says(&self, word: &str, wait: Duration) -> bool {
        let end = Instant::now() + wait;
        while let Some(left) = end.checked_duration_since(Instant::now()) {
            match self.lines.recv_timeout(left) {
                Ok(line) if line.contains(word) => return false,
                Ok(_) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        true
    }

    /// As systemd stops it.
    fn stop(&self) {
        let pid = rustix::process::Pid::from_child(&self.child);
        rustix::process::kill_process(pid, rustix::process::Signal::TERM).unwrap();
    }

    fn running(&mut self) -> bool {
        thread::sleep(Duration::from_millis(300));
        self.child.try_wait().unwrap().is_none()
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn post(port: u16, path: &str, body: &[u8], headers: &[(&str, &str)]) -> u16 {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let mut head = format!("POST {path} HTTP/1.1\r\nContent-Length: {}\r\n", body.len());
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    s.write_all(format!("{head}\r\n").as_bytes()).unwrap();
    s.write_all(body).unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    out.split(' ').nth(1).unwrap().parse().unwrap()
}

/// A delivery the service has taken and is reading: it has asked for the
/// body (100 Continue), two bytes that haven't been sent.
fn half_sent(port: u16) -> TcpStream {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    s.write_all(b"POST /burrow/hook HTTP/1.1\r\nContent-Length: 2\r\nExpect: 100-continue\r\n\r\n")
        .unwrap();
    let mut asked = [0; b"HTTP/1.1 100 Continue\r\n\r\n".len()];
    s.read_exact(&mut asked).unwrap();
    s
}

// With no handlers it still runs: exiting would be a restart every few
// seconds.
#[test]
fn no_handlers() {
    let dir = tempfile::tempdir().unwrap();
    let creds = dir.path().join("creds");
    fs::create_dir(&creds).unwrap();
    fs::write(creds.join("webhook"), "hook-secret\n").unwrap();
    let mut svc = Service::start(
        dir.path(),
        json!({"deliveries": true, "kickPort": 0, "handlers": {}}),
        &creds,
    );
    svc.said("deliveries on 127.0.0.1:");
    let kicks = svc.port("kicks on 127.0.0.1:");
    assert_eq!(post(kicks, "/kick/autoland", b"", &[]), 404);
    assert!(svc.running(), "burrow-events exited with no handlers");
}

// With no host, no deliveries: kicks only, and no secret to read.
#[test]
fn kicks_only() {
    let dir = tempfile::tempdir().unwrap();
    let mut svc = Service::start(
        dir.path(),
        json!({"deliveries": false, "kickPort": 0, "handlers": {}}),
        &dir.path().join("none"),
    );
    let line = svc.said("kicks on");
    assert!(svc.running(), "burrow-events exited without deliveries");
    assert!(!line.contains("deliveries"));
}

// No secret where deliveries need one is a failure, said: systemd restarts
// it, and the journal says why.
#[test]
fn no_secret() {
    let dir = tempfile::tempdir().unwrap();
    let mut svc = Service::start(
        dir.path(),
        json!({"deliveries": true, "kickPort": 0, "handlers": {}}),
        &dir.path().join("none"),
    );
    assert!(svc.said("burrow-events: ").contains("webhook"));
    assert!(!svc.child.wait().unwrap().success());
}

// It starts every handler's full pass when it starts, and a signed delivery
// is taken.
#[test]
fn catches_up() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("systemctl.log");
    let systemctl = dir.path().join("systemctl");
    burrow_test::executable(
        &systemctl,
        format!(
            "#!/bin/sh\necho \"$*\" >>'{}'\n[ \"$1\" = show ] && echo inactive\nexit 0\n",
            log.display()
        ),
    );
    let creds = dir.path().join("creds");
    fs::create_dir(&creds).unwrap();
    fs::write(creds.join("webhook"), "hook-secret\n").unwrap();
    let config = json!({"deliveries": true, "kickPort": 0, "handlers": {"probe": {"events": ["push"], "unit": "probe", "wake": null,
        "ownStatuses": [], "perRepo": true, "repos": null, "pushes": "any", "branches": null, "issueTitles": null}}});
    let svc = Service::start(dir.path(), config, &creds);
    let hooks = svc.port("deliveries on 127.0.0.1:");
    svc.said("probe: full pass");
    let end = Instant::now() + Duration::from_secs(10);
    while !fs::read_to_string(&log)
        .unwrap_or_default()
        .contains("start probe.service")
    {
        assert!(
            Instant::now() < end,
            "no full pass: {:?}",
            fs::read_to_string(&log)
        );
        thread::sleep(Duration::from_millis(50));
    }
    let body = br#"{"repository": {"full_name": "sam/r"}}"#;
    assert_eq!(
        post(
            hooks,
            "/burrow/hook",
            body,
            &[
                ("X-GitHub-Event", "push"),
                ("X-GitHub-Hook-ID", "42"),
                ("X-Hub-Signature-256", "sha256=00")
            ]
        ),
        401
    );
    svc.said("a push delivery from hook 42, with a signature that isn't the hook secret's");
}

// A delivery sent with no service running waits on the socket for the next.
#[test]
fn a_delivery_waits_for_the_service_across_its_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (config, creds) = landing(dir.path(), 9);
    let socket = TcpListener::bind("127.0.0.1:0").unwrap();
    socket.set_nonblocking(true).unwrap();
    let hooks = socket.local_addr().unwrap().port();
    let first = Service::start_on(dir.path(), config.clone(), &creds, &socket);
    first.said("deliveries on 127.0.0.1:");
    drop(first);
    let sent = thread::spawn(move || deliver(hooks));
    thread::sleep(Duration::from_millis(300));
    assert!(!sent.is_finished(), "refused with no service running");
    let next = Service::start_on(dir.path(), config, &creds, &socket);
    assert_eq!(sent.join().unwrap(), 202);
    drop(next);
}

// A stop answers what the service had taken, and what comes meanwhile waits
// on the socket for the next.
#[test]
fn a_stop_answers_what_it_took_and_takes_no_more() {
    let dir = tempfile::tempdir().unwrap();
    let (config, creds) = landing(dir.path(), 9);
    let socket = TcpListener::bind("127.0.0.1:0").unwrap();
    socket.set_nonblocking(true).unwrap();
    let hooks = socket.local_addr().unwrap().port();
    let mut first = Service::start_on(dir.path(), config.clone(), &creds, &socket);
    first.said("deliveries on 127.0.0.1:");
    let mut taken = half_sent(hooks);
    first.stop();
    assert!(first.running(), "it went with a delivery unanswered");
    let later = thread::spawn(move || deliver(hooks));
    thread::sleep(Duration::from_millis(300));
    taken.write_all(b"{}").unwrap();
    let mut out = String::new();
    taken.read_to_string(&mut out).unwrap();
    assert!(out.starts_with("HTTP/1.1 401 "), "{out}");
    assert!(first.child.wait().unwrap().success());
    assert!(!later.is_finished(), "a stopping service took one more");
    let next = Service::start_on(dir.path(), config, &creds, &socket);
    assert_eq!(later.join().unwrap(), 202);
    drop(next);
}

// A client that never finishes sending holds a stop up no longer than GitHub
// waits for an answer.
#[test]
fn a_stalled_client_holds_a_stop_up_only_so_long() {
    let dir = tempfile::tempdir().unwrap();
    let (config, creds) = landing(dir.path(), 9);
    let mut svc = Service::start(dir.path(), config, &creds);
    let _stalled = half_sent(svc.port("deliveries on 127.0.0.1:"));
    svc.stop();
    assert!(svc.running(), "it went with a delivery unanswered");
    // Ten seconds, which is as long as `said` waits.
    thread::sleep(Duration::from_secs(5));
    svc.said("burrow-events: stopping with a delivery unanswered");
    assert!(svc.child.wait().unwrap().success());
}

// With nothing to answer a stop is at once, deliveries or none.
#[test]
fn an_idle_stop_is_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let (hooked, creds) = landing(dir.path(), 9);
    let unhooked = json!({"deliveries": false, "kickPort": 0, "handlers": {}});
    for config in [hooked, unhooked] {
        let mut svc = Service::start(dir.path(), config, &creds);
        svc.said("kicks on");
        svc.stop();
        // Short of what a stop waits for a delivery.
        let end = Instant::now() + Duration::from_secs(5);
        while svc.running() {
            assert!(Instant::now() < end, "still up");
        }
        assert!(svc.child.wait().unwrap().success());
    }
}

// Deliveries with no socket handed over are a failure, said.
#[test]
fn no_socket() {
    let dir = tempfile::tempdir().unwrap();
    let (config, creds) = landing(dir.path(), 9);
    fs::write(dir.path().join("events.json"), config.to_string()).unwrap();
    let out = Command::new(burrow_test::program!("burrow-events"))
        .env("EVENTS_CONFIG", dir.path().join("events.json"))
        .env("CREDENTIALS_DIRECTORY", creds)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success());
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(
        said.contains("stdin isn't their listening socket"),
        "{said}"
    );
}

// A config it can't read is a failure, said.
#[test]
fn bad_config() {
    let dir = tempfile::tempdir().unwrap();
    let mut svc = Service::start(
        dir.path(),
        json!({"kickPort": 0, "handlers": {"x": {"unit": "x"}}}),
        dir.path(),
    );
    assert!(svc.said("burrow-events: ").contains("events.json"));
    assert!(!svc.child.wait().unwrap().success());
}

/// conaryd, stood in for on `listener`: answers each request 202 after
/// `delay`, and keeps its body.
fn conaryd(listener: TcpListener, delay: Duration) -> Arc<Mutex<Vec<String>>> {
    let got = Arc::new(Mutex::new(Vec::new()));
    let kept = got.clone();
    thread::spawn(move || {
        for s in listener.incoming() {
            let mut s = s.unwrap();
            let mut reader = BufReader::new(s.try_clone().unwrap());
            let mut len = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                if let Some(n) = line.strip_prefix("Content-Length: ") {
                    len = n.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; len];
            reader.read_exact(&mut body).unwrap();
            kept.lock().unwrap().push(String::from_utf8(body).unwrap());
            thread::sleep(delay);
            let _ = s.write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\n\r\n");
        }
    });
    got
}

/// The hooks' secret in these tests: made as they run, as a real one is,
/// never written in the source.
fn secret() -> &'static str {
    static SECRET: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SECRET.get_or_init(|| {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        format!("hook-{}-{nanos}", std::process::id())
    })
}

/// A directory with a landing record giving sam/r to conaryd, the webhook
/// secret, and the config forwarding to `port`.
fn landing(dir: &Path, port: u16) -> (Value, PathBuf) {
    let record = dir.join("owners.json");
    let mut r = Record::default();
    r.repos.insert(
        "sam/r".into(),
        Holding {
            owner: Owner::Conaryd,
            to: None,
            generation: 1,
            since: 0,
        },
    );
    r.write(&record).unwrap();
    let creds = dir.join("creds");
    fs::create_dir(&creds).unwrap();
    fs::write(creds.join("webhook"), format!("{}\n", secret())).unwrap();
    let outbox = dir.join("outbox");
    let config = json!({"deliveries": true, "kickPort": 0, "handlers": {},
        "landing": {"record": record, "forward": port, "outbox": outbox}});
    (config, creds)
}

const BODY: &[u8] = br#"{"repository": {"full_name": "sam/r"}}"#;

/// GitHub's delivery about sam/r, signed: its answer.
fn deliver(hooks: u16) -> u16 {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret().as_bytes()).unwrap();
    mac.update(BODY);
    let sig: String = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    post(
        hooks,
        "/burrow/hook",
        BODY,
        &[
            ("X-GitHub-Event", "push"),
            ("X-GitHub-Delivery", "d-1"),
            ("X-Hub-Signature-256", &format!("sha256={sig}")),
        ],
    )
}

/// Whether `ok` comes true within fifteen seconds.
fn soon(ok: impl Fn() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs(15);
    while Instant::now() < end {
        if ok() {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    ok()
}

fn held(dir: &Path) -> usize {
    fs::read_dir(dir.join("outbox")).map_or(0, Iterator::count)
}

// A conaryd slower to answer than GitHub waits gets the delivery once,
// GitHub hears 202, and nothing says it wasn't taken.
#[test]
fn a_slow_conaryd_takes_it_once() {
    let dir = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let got = conaryd(listener, Duration::from_secs(6));
    let (config, creds) = landing(dir.path(), port);
    let svc = Service::start(dir.path(), config, &creds);
    let hooks = svc.port("deliveries on 127.0.0.1:");
    assert_eq!(deliver(hooks), 202);
    assert!(soon(|| held(dir.path()) == 0));
    assert!(svc.never_says("not taken", Duration::from_secs(2)));
    assert_eq!(*got.lock().unwrap(), vec![String::from_utf8_lossy(BODY)]);
}

// A delivery while conaryd isn't listening is 202 and held, across a
// restart of burrow-events too, and reaches conaryd once it listens.
#[test]
fn a_delivery_waits_for_conaryd_across_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let closed = burrow_test::ClosedPort::new();
    let (config, creds) = landing(dir.path(), closed.port());
    let svc = Service::start(dir.path(), config.clone(), &creds);
    let hooks = svc.port("deliveries on 127.0.0.1:");
    assert_eq!(deliver(hooks), 202);
    svc.said("conaryd on 127.0.0.1:");
    drop(svc);
    assert_eq!(held(dir.path()), 1);

    let got = conaryd(closed.listen(), Duration::ZERO);
    let svc = Service::start(dir.path(), config, &creds);
    svc.said("deliveries on 127.0.0.1:");
    assert!(soon(|| held(dir.path()) == 0));
    assert!(svc.never_says("not taken", Duration::from_millis(500)));
    assert_eq!(*got.lock().unwrap(), vec![String::from_utf8_lossy(BODY)]);
}

// One held since before conaryd would still know its id is given up, said
// once, and never sent.
#[test]
fn a_delivery_held_too_long_is_given_up() {
    let dir = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let got = conaryd(listener, Duration::ZERO);
    let (config, creds) = landing(dir.path(), port);
    let outbox = dir.path().join("outbox");
    fs::create_dir(&outbox).unwrap();
    let head = json!({"repo": "sam/r", "received": 1.0,
        "headers": [["X-GitHub-Event", "push"], ["X-GitHub-Delivery", "d-old"]]});
    fs::write(
        outbox.join("00000000000001000000-00000000"),
        [head.to_string().as_bytes(), b"\n", BODY].concat(),
    )
    .unwrap();
    let svc = Service::start(dir.path(), config, &creds);
    let line = svc.said("sam/r: delivery d-old (push) not taken by conaryd");
    assert!(line.ends_with("never sent"), "{line}");
    assert_eq!(held(dir.path()), 0);
    assert!(svc.never_says("not taken", Duration::from_millis(500)));
    assert!(got.lock().unwrap().is_empty());
}
