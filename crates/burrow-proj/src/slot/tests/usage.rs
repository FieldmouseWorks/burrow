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
