//! The binary as the service runs it: its config, its credential, the
//! systemctl on its PATH.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

struct Service {
    child: Child,
    lines: mpsc::Receiver<String>,
}

impl Service {
    fn start(dir: &Path, config: Value, credentials: &Path) -> Service {
        let path = dir.join("events.json");
        fs::write(&path, config.to_string()).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_burrow-events"))
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
        json!({"port": 0, "kickPort": 0, "handlers": {}}),
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
        json!({"port": null, "kickPort": 0, "handlers": {}}),
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
        json!({"port": 0, "kickPort": 0, "handlers": {}}),
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
    fs::write(
        &systemctl,
        format!(
            "#!/bin/sh\necho \"$*\" >>'{}'\n[ \"$1\" = show ] && echo inactive\nexit 0\n",
            log.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&systemctl, fs::Permissions::from_mode(0o755)).unwrap();
    let creds = dir.path().join("creds");
    fs::create_dir(&creds).unwrap();
    fs::write(creds.join("webhook"), "hook-secret\n").unwrap();
    let config = json!({"port": 0, "kickPort": 0, "handlers": {"probe": {"events": ["push"], "unit": "probe", "wake": null,
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
                ("X-Hub-Signature-256", "sha256=00")
            ]
        ),
        401
    );
    svc.said("a push delivery whose signature isn't the hook secret's");
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
