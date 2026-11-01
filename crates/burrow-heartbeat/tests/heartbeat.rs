//! The beat's verdict, HOST-health as it runs, and the ping, against a
//! stand-in Healthchecks (common/mod.rs).

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use burrow_heartbeat::{Config, Health, Ping, send, verdict};
use common::Healthchecks;

fn config(fail_after: u32) -> Config {
    Config {
        name: "box-heartbeat".to_owned(),
        url: "http://127.0.0.1:9".to_owned(),
        check: "box".to_owned(),
        fail_after,
        skip: BTreeSet::from(["github".to_owned()]),
        health: PathBuf::from("/nonexistent/box-health"),
    }
}

/// One beat with these health lines.
fn beat(streaks: &Path, lines: &[&str]) -> Ping {
    let report = lines.iter().map(|l| format!("{l}\n")).collect::<String>();
    verdict(&config(2), &Health::Said(report), streaks)
}

fn ping(suffix: &'static str, body: &str) -> Ping {
    Ping {
        suffix,
        body: body.to_owned(),
    }
}

#[test]
fn a_check_fails_a_beat_only_once_it_has_failed_two_running() {
    let dir = tempfile::tempdir().unwrap();
    let s = dir.path();
    assert_eq!(
        beat(
            s,
            &["check:sshd ok", "check:github fail", "check:events ok"]
        ),
        ping("", "healthy: 2 checks pass"),
        "healthy, github skipped"
    );
    assert_eq!(
        beat(
            s,
            &[
                "check:sshd ok",
                "check:events fail",
                "unit:autoland.service fail"
            ]
        ),
        ping(
            "",
            "healthy: 1 checks pass\nfailing for less: events\nfailed units (not counted): autoland.service"
        ),
        "one bad beat, and its units"
    );
    assert_eq!(
        beat(s, &["check:sshd ok", "check:events fail", "check:zfs fail"]),
        ping(
            "/fail",
            "failing for 2 beats or more: events\nfailing for less: zfs"
        ),
        "two bad beats, and a new one"
    );
    assert_eq!(
        beat(s, &["check:sshd ok", "check:events ok", "check:zfs ok"]),
        ping("", "healthy: 3 checks pass"),
        "recovered"
    );
    assert_eq!(
        beat(s, &["check:sshd ok", "check:events fail", "check:zfs ok"]),
        ping("", "healthy: 2 checks pass\nfailing for less: events"),
        "a streak starts over"
    );
    assert_eq!(
        beat(s, &["check:sshd ok", "check:events ok", "check:zfs fail"]),
        ping("", "healthy: 2 checks pass\nfailing for less: zfs"),
        "blips taking turns"
    );
}

#[test]
fn a_failed_health_says_nothing_and_keeps_every_streak() {
    let dir = tempfile::tempdir().unwrap();
    let s = dir.path();
    beat(s, &["check:zfs fail"]);
    assert_eq!(
        verdict(
            &config(2),
            &Health::Failed("box-health failed (exit 3)".to_owned()),
            s
        ),
        ping(
            "/log",
            "box-health failed (exit 3), so this beat can't tell how the machine is"
        )
    );
    assert_eq!(
        beat(s, &["check:sshd ok", "check:zfs fail"]),
        ping("/fail", "failing for 2 beats or more: zfs"),
        "a streak across a failed HOST-health"
    );
}

#[test]
fn no_checks_at_all_is_healthy() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(beat(dir.path(), &[]), ping("", "healthy: 0 checks pass"));
}

#[test]
fn only_a_check_that_says_ok_passes() {
    let dir = tempfile::tempdir().unwrap();
    let s = dir.path();
    assert_eq!(
        beat(
            s,
            &[
                "  check:a   ok  ",
                "check:b ok then",
                "check:c",
                "check:d OK",
                "noise",
                "",
                "unit:x"
            ]
        ),
        ping(
            "",
            "healthy: 1 checks pass\nfailing for less: b c d\nfailed units (not counted): x"
        )
    );
    assert_eq!(
        beat(s, &["user-unit:music.service fail", "unit:a.service fail"]),
        ping(
            "",
            "healthy: 0 checks pass\nfailed units (not counted): music.service a.service"
        )
    );
}

#[test]
fn a_skipped_check_keeps_no_streak() {
    let dir = tempfile::tempdir().unwrap();
    let s = dir.path();
    beat(s, &["check:github fail"]);
    beat(s, &["check:github fail"]);
    assert_eq!(fs::read_dir(s).unwrap().count(), 0);
}

#[test]
fn an_unreadable_streak_starts_over() {
    let dir = tempfile::tempdir().unwrap();
    let s = dir.path();
    fs::write(s.join("zfs"), "lots\n").unwrap();
    assert_eq!(
        beat(s, &["check:zfs fail"]),
        ping("", "healthy: 0 checks pass\nfailing for less: zfs")
    );
    assert_eq!(fs::read_to_string(s.join("zfs")).unwrap(), "1\n");
}

#[test]
fn a_streak_it_cant_keep_counts_as_down() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("not-a-directory");
    fs::write(&file, "").unwrap();
    assert_eq!(
        beat(&file, &["check:zfs fail", "check:sshd ok"]),
        ping("/fail", "failing for 2 beats or more: zfs")
    );
    let dir = tempfile::tempdir().unwrap();
    let run = dir.path().join("run");
    fs::create_dir(&run).unwrap();
    assert_eq!(
        beat(
            &run,
            &["check:.. fail", "check:a/b fail", "check:../escaped fail"]
        ),
        ping("/fail", "failing for 2 beats or more: .. a/b ../escaped"),
        "names that can't be a file's"
    );
    assert!(
        !dir.path().join("escaped").exists(),
        "a streak written outside its directory"
    );
}

#[test]
fn failing_after_one_beat_fails_the_first() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        verdict(
            &config(1),
            &Health::Said("check:zfs fail\n".to_owned()),
            dir.path()
        ),
        ping("/fail", "failing for 1 beats or more: zfs")
    );
}

/// A HOST-health that runs `script`, named `box-health`.
fn health(dir: &Path, script: &str) -> PathBuf {
    let path = dir.join("box-health");
    fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[test]
fn health_is_its_whole_report_or_how_it_failed() {
    let dir = tempfile::tempdir().unwrap();
    let said = |script: &str| match Health::run(&health(dir.path(), script)) {
        Health::Said(report) => format!("said {report:?}"),
        Health::Failed(why) => format!("failed: {why}"),
    };
    assert_eq!(
        said("echo check:a ok; echo check:b fail"),
        "said \"check:a ok\\ncheck:b fail\\n\""
    );
    assert_eq!(
        said("echo check:a ok; exit 3"),
        "failed: box-health failed (exit 3)"
    );
    assert_eq!(
        said("echo check:a ok; kill -9 $$"),
        "failed: box-health failed (exit 137)"
    );
    let gone = dir.path().join("gone-health");
    let Health::Failed(why) = Health::run(&gone) else {
        panic!("a missing HOST-health ran")
    };
    assert!(why.starts_with("gone-health couldn't run ("), "{why}");
}

/// An agent like the beat's, with no proxy and a short try.
fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .proxy(None)
        .timeout_global(Some(Duration::from_millis(500)))
        .http_status_as_error(false)
        .build()
        .into()
}

/// Sends `p` to the stand-in; returns what `send` said and its pauses.
fn sent(hc: &Healthchecks, key: &str, p: &Ping) -> (Result<(), String>, Vec<u64>) {
    let mut pauses = Vec::new();
    let r = send(&agent(), &hc.url, key, "box", p, &mut |d| {
        pauses.push(d.as_secs())
    });
    (r, pauses)
}

#[test]
fn a_ping_goes_to_its_check_with_its_body() {
    let hc = Healthchecks::serve("/KEY/box", &[]);
    assert_eq!(
        sent(&hc, "KEY", &ping("", "healthy: 2 checks pass")),
        (Ok(()), vec![])
    );
    assert_eq!(
        hc.last(),
        ("/KEY/box".to_owned(), "healthy: 2 checks pass\n".to_owned())
    );
    sent(
        &hc,
        "KEY",
        &ping(
            "/fail",
            "failing for 2 beats or more: zfs\nfailing for less: a",
        ),
    )
    .0
    .unwrap();
    assert_eq!(
        hc.last(),
        (
            "/KEY/box/fail".to_owned(),
            "failing for 2 beats or more: zfs\nfailing for less: a\n".to_owned()
        )
    );
    let slash = Healthchecks::serve("/KEY/box", &[]);
    let url = format!("{}/", slash.url);
    send(
        &agent(),
        &url,
        "KEY",
        "box",
        &ping("/log", "x"),
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(
        slash.last().0,
        "/KEY/box/log",
        "a trailing slash on the URL"
    );
}

#[test]
fn a_ping_healthchecks_couldnt_take_is_tried_three_times_more() {
    let hc = Healthchecks::serve("/KEY/box", &[503, 429]);
    assert_eq!(sent(&hc, "KEY", &ping("", "x")), (Ok(()), vec![1, 2]));
    assert_eq!(hc.pings().len(), 3);
    let hc = Healthchecks::serve("/KEY/box", &[500, 502, 504, 408]);
    assert_eq!(
        sent(&hc, "KEY", &ping("", "x")),
        (Err("HTTP 408".to_owned()), vec![1, 2, 4])
    );
    assert_eq!(hc.pings().len(), 4);
}

#[test]
fn a_ping_healthchecks_refuses_is_not_tried_again() {
    let hc = Healthchecks::serve("/KEY/box", &[]);
    assert_eq!(
        sent(&hc, "NOTKEY", &ping("", "x")),
        (Err("HTTP 404".to_owned()), vec![])
    );
    assert_eq!(hc.pings().len(), 1);
}

#[test]
fn a_ping_that_cant_connect_is_not_tried_again_and_never_names_the_key() {
    let mut pauses = 0;
    let r = send(
        &agent(),
        &common::nobody(),
        "SECRET",
        "box",
        &ping("", "x"),
        &mut |_| pauses += 1,
    );
    let e = r.unwrap_err();
    assert!(!e.contains("SECRET"), "{e}");
    assert_eq!(pauses, 0, "{e}");
}

#[test]
fn a_ping_that_times_out_is_tried_again() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    // Takes every connection and never answers.
    std::thread::spawn(move || {
        let held: Vec<_> = listener.incoming().flatten().collect();
        drop(held);
    });
    let mut pauses = Vec::new();
    let r = send(&agent(), &url, "SECRET", "box", &ping("", "x"), &mut |d| {
        pauses.push(d.as_secs())
    });
    let e = r.unwrap_err();
    assert_eq!(pauses, vec![1, 2, 4], "{e}");
    assert!(!e.contains("SECRET"), "{e}");
}

#[test]
fn a_key_that_cant_be_in_a_url_isnt_named_either() {
    let r = send(
        &agent(),
        &common::nobody(),
        "SE CRET",
        "box",
        &ping("", "x"),
        &mut |_| {},
    );
    let e = r.unwrap_err();
    assert!(!e.contains("CRET"), "{e}");
}

#[test]
fn an_empty_key_pings_nothing() {
    let hc = Healthchecks::serve("/KEY/box", &[]);
    assert_eq!(
        sent(&hc, "", &ping("", "x")),
        (Err("the ping key is empty".to_owned()), vec![])
    );
    assert!(hc.pings().is_empty());
}
