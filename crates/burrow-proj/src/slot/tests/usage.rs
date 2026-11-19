use super::*;

#[test]
fn fresh_meter_runs_the_held_turn_and_closes_its_notice() {
    use burrow_usage_reset::meter::{FRESH, Snapshot, Window};
    let tmp = TempDir::new().unwrap();
    let jobs = jobs_dir(&tmp);
    let notices = jobs.join("notices");
    burrow_test::executable(
        jobs.join("notify"),
        format!("#!/bin/sh\necho \"$1 $2\" >> '{}'\n", notices.display()),
    );
    let cfg = config(&jobs, 1);
    let t = now();
    crate::usage::record(
        &cfg,
        "w0",
        Some(t + 7200),
        "usage limit",
        t - FRESH as i64 - 100,
    )
    .unwrap();
    let _kid = waiter(&jobs, &tmp, "w0", 1);
    until("held turn joins the queue", || in_queue(&jobs) == 1);
    let mut meter = Snapshot {
        windows: [(
            "seven_day".into(),
            Window {
                used: 1.0,
                resets_at: 0.0,
            },
        )]
        .into(),
        credits: false,
        read_at: t as f64 - FRESH - 1.0,
        requested_at: Some(t as f64 - FRESH - 1.0),
    };
    meter
        .record(cfg.meters.as_ref().unwrap(), "claude")
        .unwrap();
    sleep(Duration::from_secs(3));
    assert_eq!(took(&jobs, "w0"), None, "stale meter keeps the hold");
    crate::usage::record(&cfg, "w0", Some(t + 7200), "usage limit", t - 10).unwrap();
    meter.read_at = (t - 11) as f64;
    meter.requested_at = Some(meter.read_at);
    meter
        .record(cfg.meters.as_ref().unwrap(), "claude")
        .unwrap();
    sleep(Duration::from_secs(3));
    assert_eq!(took(&jobs, "w0"), None, "pre-refusal meter keeps the hold");
    meter.read_at = now() as f64;
    meter.requested_at = Some(meter.read_at);
    meter
        .record(cfg.meters.as_ref().unwrap(), "claude")
        .unwrap();
    until("fresh meter lets the held turn run", || {
        took(&jobs, "w0").is_some()
    });
    assert!(crate::usage::out(&cfg).is_none());
    assert!(
        fs::read_to_string(notices)
            .unwrap()
            .contains("close proj-usage")
    );
}

// This machine's login is a credential a sandbox doesn't use (usage/login.rs):
// while its hold stands, the turns here wait in line, and a sandbox's takes
// the free slot past them, as its own --unstick turn does.
#[test]
fn a_sandboxs_turn_takes_a_slot_past_this_machines_login_hold() {
    use crate::engine::{Engine, claude::LOGGED_OUT};
    let tmp = TempDir::new().unwrap();
    let jobs = jobs_dir(&tmp);
    let cfg = config(&jobs, 1);
    crate::usage::record(&cfg, "here", None, LOGGED_OUT, now()).unwrap();
    let _here = waiter(&jobs, &tmp, "here", 1);
    until("the turn here joins the line", || in_queue(&jobs) == 1);
    sleep(Duration::from_secs(3));
    assert_eq!(took(&jobs, "here"), None, "the login hold holds it");
    // Room for a sandbox's turn, with the one here in line, and none for
    // another here.
    assert!(crate::slot::room(&cfg, Engine::Claude, true));
    assert!(!crate::slot::room(&cfg, Engine::Claude, false));
    fs::create_dir_all(jobs.join("vm")).unwrap();
    fs::write(jobs.join("vm/agent"), "web-ab12\n").unwrap();
    let _vm = waiter(&jobs, &tmp, "vm", 1);
    until("the sandbox's turn takes the free slot", || {
        took(&jobs, "vm").is_some()
    });
    assert_eq!(took(&jobs, "here"), None);
    assert!(crate::usage::out(&cfg).is_some(), "the hold still stands");

    fs::create_dir_all(jobs.join("u")).unwrap();
    fs::write(jobs.join("u/project"), "/src/web\n").unwrap();
    fs::write(jobs.join("u/unstick"), "").unwrap();
    fs::write(jobs.join("u/agent"), "web-cd34\n").unwrap();
    admission::bind_fixture(&jobs, 1);
    let unstick = std::thread::spawn(move || take(&cfg, "u", "1", Class::Normal, true).unwrap());
    until("the sandbox's --unstick turn takes its own slot", || {
        unstick.is_finished()
    });
    let slot = unstick.join().unwrap();
    assert_eq!(
        fs::read_to_string(jobs.join("u/1.slot")).unwrap(),
        "unstick\n"
    );
    slot.unlock().unwrap();
}
