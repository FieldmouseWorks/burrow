//! A site's watch, `burrow-heartbeat --site CONFIG`, as its unit runs it:
//! the ping key as a credential, a stand-in site and a stand-in Healthchecks
//! (common/mod.rs), both over HTTP.

mod common;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use burrow_heartbeat::site::answers;
use common::Healthchecks;

const KEY: &str = "SITEPINGKEY";

struct Unit {
    _dir: tempfile::TempDir,
    root: PathBuf,
    creds: PathBuf,
}

impl Unit {
    fn new() -> Unit {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_owned();
        let creds = root.join("creds");
        fs::create_dir(&creds).unwrap();
        fs::write(creds.join("ping-key"), format!(" {KEY} \n")).unwrap();
        Unit {
            _dir: dir,
            root,
            creds,
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_burrow-heartbeat"));
        cmd.args(args)
            .env("CREDENTIALS_DIRECTORY", &self.creds)
            .env("RUNTIME_DIRECTORY", &self.root);
        for var in [
            "ALL_PROXY",
            "all_proxy",
            "HTTPS_PROXY",
            "https_proxy",
            "HTTP_PROXY",
            "http_proxy",
        ] {
            cmd.env_remove(var);
        }
        cmd
    }

    /// The watch of `site`, pinging `url`.
    fn watching(&self, url: &str, site: &str) -> Command {
        let config = self.root.join("box-heartbeat-site-web.json");
        let json = serde_json::json!({
            "name": "box-heartbeat-site-web", "url": url, "check": "web", "site": site,
        });
        fs::write(&config, json.to_string()).unwrap();
        self.command(&["--site", config.to_str().unwrap()])
    }

    /// One watch of `site`, pinging `url`.
    fn watch(&self, url: &str, site: &str) -> Output {
        self.watching(url, site).output().unwrap()
    }

    /// One beat, pinging `url`, with a HOST-health that says nothing: the
    /// path the machine's own ping takes.
    fn beat(&self, url: &str) -> Output {
        let health = self.root.join("box-health");
        fs::write(&health, "#!/bin/sh\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&health, fs::Permissions::from_mode(0o755)).unwrap();
        let config = self.root.join("box-heartbeat.json");
        let json = serde_json::json!({
            "name": "box-heartbeat", "url": url, "check": "web", "failAfter": 2,
            "skip": [], "health": health,
        });
        fs::write(&config, json.to_string()).unwrap();
        self.command(&[config.to_str().unwrap()]).output().unwrap()
    }
}

fn said(out: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// A site that answers `code` to every fetch, saying `Location: moved_to`.
fn site(code: u16, moved_to: Option<&str>) -> (Healthchecks, String) {
    let codes = vec![code; 4];
    let s = Healthchecks::serve_moved("/health", &codes, moved_to);
    let url = format!("{}/health", s.url);
    (s, url)
}

#[test]
fn a_site_answering_2xx_pings_its_check() {
    for code in [200, 201, 204, 299] {
        let hc = Healthchecks::serve("/SITEPINGKEY/web", &[]);
        let (s, url) = site(code, None);
        let out = Unit::new().watch(&hc.url, &url);
        assert!(out.status.success(), "{code} {:?}", said(&out));
        assert_eq!(
            said(&out),
            (format!("web: {url} answers HTTP {code}\n"), String::new()),
            "{code}"
        );
        assert_eq!(s.pings(), [("/health".to_owned(), String::new())], "{code}");
        assert_eq!(
            hc.pings(),
            [(
                "/SITEPINGKEY/web".to_owned(),
                format!("{url} answers HTTP {code}\n")
            )],
            "{code}"
        );
    }
}

/// Only a 2xx is an answer: a redirect isn't followed, wherever it points
/// (a healthy page included), and pings nothing, nor does any other status.
#[test]
fn a_redirect_or_any_other_status_pings_nothing() {
    let (elsewhere, healthy) = site(200, None);
    for code in [300, 301, 302, 303, 304, 307, 308, 399, 404, 500, 503] {
        for moved_to in [Some(healthy.as_str()), None] {
            let hc = Healthchecks::serve("/SITEPINGKEY/web", &[]);
            let (_s, url) = site(code, moved_to);
            let out = Unit::new().watch(&hc.url, &url);
            assert!(out.status.success(), "{code} {moved_to:?}");
            let (o, e) = said(&out);
            assert_eq!(e, "", "{code} {moved_to:?}");
            assert!(
                o.starts_with(&format!("web: {url} didn't answer 2xx ("))
                    && o.ends_with("), so no ping\n"),
                "{code} {moved_to:?}: {o}"
            );
            assert!(
                hc.pings().is_empty(),
                "{code} {moved_to:?}: {:?}",
                hc.pings()
            );
        }
    }
    assert!(elsewhere.pings().is_empty(), "a redirect was followed");
}

#[test]
fn a_site_that_cant_be_reached_pings_nothing() {
    let hc = Healthchecks::serve("/SITEPINGKEY/web", &[]);
    for url in [common::nobody(), "not a url".to_owned()] {
        let out = Unit::new().watch(&hc.url, &url);
        assert!(out.status.success(), "{url}");
        let (o, e) = said(&out);
        assert!(
            o.starts_with(&format!("web: {url} didn't answer 2xx (")),
            "{o}"
        );
        assert_eq!(e, "", "{url}");
    }
    assert!(hc.pings().is_empty(), "{:?}", hc.pings());
}

/// A 200 whose body breaks off before its Content-Length isn't a completed
/// answer.
#[test]
fn a_2xx_that_breaks_off_pings_nothing() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/health", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut line = String::new();
            let mut reader = BufReader::new(&stream);
            while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                line.clear();
            }
            let mut stream = stream;
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nshort"
            );
        }
    });
    let hc = Healthchecks::serve("/SITEPINGKEY/web", &[]);
    let out = Unit::new().watch(&hc.url, &url);
    assert!(out.status.success());
    let (o, _) = said(&out);
    assert!(
        o.starts_with(&format!(
            "web: {url} didn't answer 2xx (HTTP 200, but its body broke off ("
        )),
        "{o}"
    );
    assert!(hc.pings().is_empty(), "{:?}", hc.pings());
}

#[test]
fn a_ping_that_doesnt_land_is_said_without_the_key_and_fails_nothing() {
    let (_s, url) = site(200, None);
    let refused = Healthchecks::serve("/OTHERKEY/web", &[]);
    let out = Unit::new().watch(&refused.url, &url);
    assert!(out.status.success(), "{:?}", said(&out));
    assert_eq!(
        said(&out),
        (
            format!("web: {url} answers HTTP 200\n"),
            "web: the ping to web didn't land (HTTP 404)\n".to_owned()
        )
    );
    for ping_url in [common::nobody(), "/SITEPINGKEY/relative".to_owned()] {
        let out = Unit::new().watch(&ping_url, &url);
        assert!(out.status.success(), "{ping_url}");
        let (o, e) = said(&out);
        assert!(e.starts_with("web: the ping to web didn't land ("), "{e}");
        assert!(!o.contains(KEY) && !e.contains(KEY), "{o}{e}");
    }
}

/// The site's ping goes the way the beat's does: tried again when
/// Healthchecks couldn't take it.
#[test]
fn a_ping_healthchecks_couldnt_take_is_tried_again() {
    let (_s, url) = site(200, None);
    let hc = Healthchecks::serve("/SITEPINGKEY/web", &[503]);
    let out = Unit::new().watch(&hc.url, &url);
    assert_eq!(said(&out).1, "", "{:?}", said(&out));
    assert_eq!(hc.pings().len(), 2);
}

/// Zero, one or more trailing slashes on the endpoint, with or without a
/// path prefix, give the site's ping the path the machine's own takes.
#[test]
fn the_endpoint_reads_the_same_for_a_site_as_for_the_beat() {
    let (_s, url) = site(200, None);
    for prefix in ["", "/hc"] {
        let check = format!("{prefix}/SITEPINGKEY/web");
        for slashes in ["", "/", "//", "///"] {
            let hc = Healthchecks::serve(&check, &[]);
            let endpoint = format!("{}{prefix}{slashes}", hc.url);
            let unit = Unit::new();
            let out = unit.watch(&endpoint, &url);
            assert_eq!(said(&out).1, "", "{endpoint}");
            let out = unit.beat(&endpoint);
            assert_eq!(said(&out).1, "", "{endpoint}");
            let paths: Vec<String> = hc.pings().into_iter().map(|(p, _)| p).collect();
            assert_eq!(paths, [check.clone(), check.clone()], "{endpoint}");
        }
    }
}

/// While the ping is on its way, neither the watch nor anything it runs has
/// the key in its arguments, where any local user could read it
/// (/proc/PID/cmdline).
#[test]
fn the_key_is_in_no_process_arguments() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let hc = format!("http://{}", listener.local_addr().unwrap());
    let watcher = Arc::new(AtomicU32::new(0));
    let seen: Arc<Mutex<Option<Vec<String>>>> = Arc::default();
    let (kept, pid) = (Arc::clone(&seen), Arc::clone(&watcher));
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            while pid.load(Ordering::SeqCst) == 0 {
                std::thread::yield_now();
            }
            *kept.lock().unwrap() = Some(holding_the_key(pid.load(Ordering::SeqCst)));
            let mut reader = BufReader::new(&stream);
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                line.clear();
            }
            let mut stream = stream;
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
        }
    });
    let (_s, url) = site(200, None);
    let unit = Unit::new();
    let child = unit
        .watching(&hc, &url)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    watcher.store(child.id(), Ordering::SeqCst);
    let out = child.wait_with_output().unwrap();
    assert_eq!(said(&out).1, "", "{:?}", said(&out));
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen, Some(vec![]), "the key in a process's arguments");
}

/// The command lines of `pid` and every process under it that name the key.
fn holding_the_key(pid: u32) -> Vec<String> {
    let mut parents = Vec::new();
    for entry in fs::read_dir("/proc").unwrap().flatten() {
        let Ok(n) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        // Its parent is the field after the name, which ends at the last ')'.
        let parent = fs::read_to_string(entry.path().join("stat"))
            .ok()
            .and_then(|stat| {
                let (_, rest) = stat.rsplit_once(')')?;
                rest.split_whitespace().nth(1)?.parse::<u32>().ok()
            });
        if let Some(parent) = parent {
            parents.push((n, parent));
        }
    }
    let mut under = vec![pid];
    let mut i = 0;
    while i < under.len() {
        let p = under[i];
        under.extend(parents.iter().filter(|(_, q)| *q == p).map(|(n, _)| *n));
        i += 1;
    }
    under
        .iter()
        .filter_map(|n| fs::read(format!("/proc/{n}/cmdline")).ok())
        .map(|c| String::from_utf8_lossy(&c).replace('\0', " "))
        .filter(|c| c.contains(KEY))
        .collect()
}

#[test]
fn answers_is_2xx_and_its_whole_body() {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .proxy(None)
        .timeout_global(Some(Duration::from_millis(500)))
        .http_status_as_error(false)
        .max_redirects(0)
        .build()
        .into();
    let (_s, ok) = site(200, None);
    assert_eq!(answers(&agent, &ok), Ok(200));
    let (_s, moved) = site(302, Some(&ok));
    assert_eq!(answers(&agent, &moved), Err("HTTP 302".to_owned()));
    let (_s, gone) = site(404, None);
    assert_eq!(answers(&agent, &gone), Err("HTTP 404".to_owned()));
}

#[test]
fn outside_its_unit_it_says_so() {
    let unit = Unit::new();
    let out = unit.command(&["--site", "/nonexistent"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2), "{:?}", said(&out));
    let config = unit.root.join("site.json");
    fs::write(
        &config,
        r#"{"name":"box-heartbeat-site-web","url":"x","check":"web","site":"x"}"#,
    )
    .unwrap();
    let out = unit
        .command(&["--site", config.to_str().unwrap()])
        .env_remove("CREDENTIALS_DIRECTORY")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        said(&out).1,
        "box-heartbeat-site-web: CREDENTIALS_DIRECTORY is unset: run it as the box-heartbeat-site-web unit\n"
    );
    for args in [&["--site"][..], &["--beat", "x"], &["--site", "a", "b"]] {
        let out = unit.command(args).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}");
    }
}
