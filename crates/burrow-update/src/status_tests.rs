//! What a run says it did, and `status` reads back.

use std::fs;

use serde_json::json;

use super::*;
use crate::testing::Machine;

#[test]
fn a_run_keeps_what_it_did_whole() {
    let m = Machine::new();
    let a = m.system("a", "");
    m.runs(&a);
    note(&m.cfg, Outcome::Held, "holding", Some(7));
    let s = m.status();
    assert_eq!(
        (&s["result"], &s["message"], &s["running"], &s["behind"]),
        (&json!("held"), &json!("holding"), &json!(a), &json!(7))
    );
    // As date -Is says it: to the second, with the zone's offset.
    let time = s["time"].as_str().unwrap();
    assert!(
        time.len() == 25 && time.as_bytes()[10] == b'T' && time.as_bytes()[22] == b':',
        "{time}"
    );
    // A failure is the run's own to fix: nothing it's behind.
    note(&m.cfg, Outcome::Failed, "broke", Some(7));
    assert_eq!(m.status().get("behind"), None);
    note(&m.cfg, Outcome::Ok, "live", None);
    assert_eq!(m.status().get("behind"), None);
    assert!(!m.cfg.state.join("status.json.new").exists());
    // What runs, unread: nothing.
    fs::remove_file(&m.cfg.current_system).unwrap();
    note(&m.cfg, Outcome::Current, "up to date", None);
    assert_eq!(m.status()["running"], json!(""));
}

#[test]
fn status_says_the_last_run_and_whats_queued() {
    let mut m = Machine::new();
    let timers = "\
Sun 2026-11-01 10:00:00 PST 5min -    -      burrow-events-catch-up.timer burrow-events-catch-up.service
Mon 2026-11-02 03:00:00 PST 17h  -    -      box-boot-trial.timer         box-boot-trial.service
";
    assert_eq!(status(&m.cfg), format!("no run yet\n{timers}"));
    assert_eq!(
        m.did("calls"),
        "systemctl list-timers burrow-events-catch-up.timer --no-pager\n"
    );

    fs::write(
        m.cfg.state.join("status.json"),
        json!({"time": "2026-11-01T09:00:00-07:00", "result": "held", "message": "holding"})
            .to_string(),
    )
    .unwrap();
    let since = jiff::Timestamp::from_second(1_700_000_000).unwrap();
    fs::write(m.cfg.state.join("held-since"), "1700000000\n").unwrap();
    fs::write(m.cfg.state.join("jobs/build-main"), "homelab-3\n").unwrap();
    fs::write(m.cfg.state.join("jobs/check-vm"), "homelab-4\n").unwrap();
    m.cfg.tools.boot = Some(m.path("bin/systemctl").with_file_name("box-boot"));
    std::os::unix::fs::symlink(
        crate::testing::bin().join("boot"),
        m.cfg.tools.boot.as_ref().unwrap(),
    )
    .unwrap();
    fs::write(m.path("boot-status"), "trial queued: /nix/store/k\n").unwrap();
    assert_eq!(
        status(&m.cfg),
        format!(
            "last run 2026-11-01T09:00:00-07:00: held: holding\nheld since {}\nfix job for build-main: homelab-3\nfix job for check-vm: homelab-4\ntrial queued: /nix/store/k\n{timers}",
            iso(since)
        )
    );
    // Something that isn't its status is no run.
    fs::write(m.cfg.state.join("status.json"), "{").unwrap();
    assert!(status(&m.cfg).starts_with("no run yet\n"));
}
