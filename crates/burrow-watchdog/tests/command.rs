//! The command as the machine runs it: its config, and proj, notify and
//! burrow-events' kick port for real processes and sockets.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

use burrow_events::http::listen;
use serde_json::json;

fn script(path: &Path, body: &str) {
    fs::write(path, format!("#!/bin/sh\n{body}")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn watchdog(config: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_burrow-watchdog"))
        .args(args)
        .env("WATCHDOG_CONFIG", config)
        .output()
        .unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

// A pass kicks one stall's pass and hands the other to a job; status shows
// it all and changes nothing; snooze gives one more time, and takes only
// what it should.
#[test]
fn command() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let (state, watch) = (d.join("state"), d.join("steward.json"));
    fs::create_dir(&state).unwrap();
    script(
        &d.join("proj"),
        &format!(
            "echo \"$*\" >>'{log}'\ncase $1 in run) echo job-1 ;; state) echo running ;; esac\n",
            log = d.join("proj.log").display()
        ),
    );
    script(&d.join("notify"), "[ \"$1\" != pending ] || exit 1\n");
    let kicks = Arc::new(Mutex::new(Vec::new()));
    let seen = kicks.clone();
    let (port, _) = listen(
        0,
        Arc::new(move |req| {
            seen.lock().unwrap().push(req.target.clone());
            202
        }),
    )
    .unwrap();
    let config = d.join("config.json");
    fs::write(
        &config,
        json!({
            "stateDir": state, "proj": d.join("proj"), "project": "homelab", "jobTimeout": 60, "jobIdle": 20,
            "sources": {"autoland": {"file": watch, "about": "the PR steward", "look": "journalctl -u autoland.",
                                     "kick": "autoland", "stale": null}},
            "kick": format!("http://127.0.0.1:{port}/kick"), "escalateAfter": 7200, "ownerAfter": 14400,
            "stewardKick": "autoland", "labels": null, "notify": d.join("notify"), "owner": "Sam", "host": "box",
        })
        .to_string(),
    )
    .unwrap();
    let now = burrow_watchdog::now();
    let item = |what: &str, kick| {
        json!({"what": what, "link": null, "phase": "waiting for CI", "since": now - 3.0 * 3600.0,
               "due": now - 60.0, "kick": kick})
    };
    fs::write(
        &watch,
        json!({"at": now, "items": {"sam/r#7": item("PR #7", Some("autoland/sam/r")), "sam/q#2": item("PR #2", None)}})
            .to_string(),
    )
    .unwrap();

    let status = watchdog(&config, &["status"]);
    assert!(status.status.success(), "{}", text(&status.stderr));
    assert!(
        text(&status.stdout).contains("OVERDUE sam/r#7: waiting for CI"),
        "{}",
        text(&status.stdout)
    );
    assert!(!state.join("state.json").exists() && kicks.lock().unwrap().is_empty());

    let pass = watchdog(&config, &[]);
    assert!(pass.status.success(), "{}", text(&pass.stderr));
    assert_eq!(*kicks.lock().unwrap(), ["/kick/autoland/sam/r"]);
    let log = fs::read_to_string(d.join("proj.log")).unwrap();
    assert!(
        log.starts_with("run --pipeline --timeout 60m --idle 20m --kick watchdog --owned homelab "),
        "{log}"
    );
    assert!(log.contains("PR #2") && !log.contains("PR #7"), "{log}");
    let wake: serde_json::Value =
        serde_json::from_slice(&fs::read(state.join("wake.json")).unwrap()).unwrap();
    assert!(wake[""].as_f64().is_some(), "{wake}");

    let snoozed = watchdog(
        &config,
        &["snooze", "autoland", "sam/q#2", "30", "a long build"],
    );
    assert!(snoozed.status.success(), "{}", text(&snoozed.stderr));
    assert!(text(&snoozed.stdout).starts_with("snoozed until "));
    let status = text(&watchdog(&config, &["status"]).stdout);
    assert!(
        status.contains("stalled (more time: a long build): PR #2"),
        "{status}"
    );
    assert!(status.contains("stalled (kicked): PR #7"), "{status}");

    let twice = watchdog(&config, &["snooze", "autoland", "sam/q#2", "30", "again"]);
    assert!(!twice.status.success() && text(&twice.stderr).contains("once already"));
    for bad in [
        &["snooze", "autoland", "x", "soon", "why"][..],
        &["status", "now"],
        &["pass"],
    ] {
        let out = watchdog(&config, bad);
        assert!(
            !out.status.success() && text(&out.stderr).contains("usage"),
            "{bad:?}"
        );
    }
}

// A config it can't read is a failure that names it.
#[test]
fn bad_config() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.json");
    fs::write(&config, r#"{"stateDir": "/x"}"#).unwrap();
    let out = watchdog(&config, &[]);
    assert!(
        !out.status.success() && text(&out.stderr).contains("config.json"),
        "{}",
        text(&out.stderr)
    );
}
