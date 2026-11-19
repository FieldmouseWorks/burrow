// What releases a hold from a meter reading: only a fresh one, of Claude's,
// read wholly after the last refusal, with room in every window.

use std::fs;

use serde_json::json;

use super::*;

#[test]
fn scheduling_releases_only_a_fresh_meter_after_the_refusal() {
    use burrow_usage_reset::meter::{FRESH, Snapshot, Window};
    let (d, mut cfg) = rig();
    cfg.meters = Some(d.path().join("meters"));
    cfg.pipeline_slots = 0;
    cfg.min_free_memory = 0;
    let t = now();
    record(&cfg, "held", Some(t + 7200), SAID, t - FRESH as i64 - 100).unwrap();
    let mut meter = Snapshot {
        windows: [
            (
                "seven_day".into(),
                Window {
                    used: 1.0,
                    resets_at: 0.0,
                },
            ),
            (
                "five_hour".into(),
                Window {
                    used: 6.0,
                    resets_at: 0.0,
                },
            ),
        ]
        .into(),
        credits: false,
        read_at: t as f64,
        requested_at: Some(t as f64),
    };
    // A stale reading after the hold began still proves nothing.
    meter.read_at = t as f64 - FRESH - 1.0;
    meter.requested_at = Some(meter.read_at);
    meter
        .record(cfg.meters.as_ref().unwrap(), "claude")
        .unwrap();
    assert!(!crate::slot::room(
        &cfg,
        crate::engine::Engine::Claude,
        false
    ));
    // A later refusal supersedes the original hold's start.
    record(&cfg, "held", Some(t + 7200), SAID, t - 10).unwrap();
    for at in [(t - 11) as f64, (t - 10) as f64, (t - 10) as f64 + 0.2] {
        meter.read_at = at;
        meter.requested_at = Some(at);
        meter
            .record(cfg.meters.as_ref().unwrap(), "claude")
            .unwrap();
        assert!(!crate::slot::room(
            &cfg,
            crate::engine::Engine::Claude,
            false
        ));
        assert!(out(&cfg).is_some());
        assert_eq!(told(&d).len(), 1);
    }
    meter.read_at = t as f64;
    meter.requested_at = Some(meter.read_at);
    // Every window must have room.
    meter.windows.get_mut("five_hour").unwrap().used = 100.0;
    meter
        .record(cfg.meters.as_ref().unwrap(), "claude")
        .unwrap();
    assert!(!crate::slot::room(
        &cfg,
        crate::engine::Engine::Claude,
        false
    ));
    meter.windows.get_mut("five_hour").unwrap().used = 6.0;
    meter
        .record(cfg.meters.as_ref().unwrap(), "claude")
        .unwrap();
    assert!(
        crate::slot::room(&cfg, crate::engine::Engine::Claude, false),
        "next scheduling pass releases the held queue"
    );
    let job = Job::new(&cfg, "held");
    fs::create_dir_all(&job.dir).unwrap();
    crate::policy::bind(
        &job,
        crate::policy::Identity::Local,
        crate::policy::Kind::Manual,
    )
    .unwrap();
    let _slot = crate::slot::take(&cfg, "held", "1", crate::pick::Class::Normal, false).unwrap();
    assert!(job.turn("1", "slot").exists());
    assert!(out(&cfg).is_none());
    assert!(told(&d).last().unwrap().starts_with("close proj-usage"));
}

#[test]
fn a_fractional_meter_in_the_initial_refusal_second_keeps_the_hold() {
    let (d, mut cfg) = rig();
    cfg.meters = Some(d.path().join("meters"));
    let t = now() - 10;
    record(&cfg, "held", Some(t + 7200), SAID, t).unwrap();
    fs::create_dir_all(cfg.meters.as_ref().unwrap()).unwrap();
    fs::write(
        cfg.meters.as_ref().unwrap().join("claude.json"),
        json!({
            "windows": {"seven_day": {"used": 1.0, "resets_at": 0.0}},
            "credits": false, "read_at": t as f64 + 0.2, "requested_at": t as f64 + 0.2
        })
        .to_string(),
    )
    .unwrap();
    tend(&cfg);
    assert!(out(&cfg).is_some(), "same-second ordering is ambiguous");
    assert_eq!(told(&d).len(), 1);
}

#[test]
fn refusal_between_meter_check_and_locked_release_keeps_the_hold() {
    let (d, mut cfg) = rig();
    cfg.meters = Some(d.path().join("meters"));
    let t = now() - 10;
    record(&cfg, "held", Some(t + 7200), SAID, t).unwrap();
    fs::create_dir_all(cfg.meters.as_ref().unwrap()).unwrap();
    fs::write(
        cfg.meters.as_ref().unwrap().join("claude.json"),
        json!({
            "windows": {"seven_day": {"used": 1.0, "resets_at": 0.0}},
            "credits": false, "read_at": (t + 5) as f64 + 0.2, "requested_at": (t + 5) as f64 + 0.2
        })
        .to_string(),
    )
    .unwrap();
    let started = meter_start(&cfg).unwrap();
    // Another turn is refused after the initial check but before back locks.
    record(&cfg, "other", Some(t + 7200), SAID, t + 5).unwrap();
    back(&cfg, Some(started));
    assert!(out(&cfg).is_some());
    assert_eq!(told(&d).len(), 1);
}

#[test]
fn a_completed_read_does_not_prove_when_its_request_began() {
    let (d, mut cfg) = rig();
    cfg.meters = Some(d.path().join("meters"));
    let t = now();
    record(&cfg, "held", Some(t + 7200), SAID, t - 10).unwrap();
    fs::create_dir_all(cfg.meters.as_ref().unwrap()).unwrap();
    for requested in [
        None,
        Some((t - 11) as f64),
        Some(t as f64 - 3601.0),
        Some((t + 1) as f64),
    ] {
        // Older producer, in-flight request, stale request, or invalid ordering.
        fs::write(
            cfg.meters.as_ref().unwrap().join("claude.json"),
            json!({
                "windows": {"seven_day": {"used": 1.0, "resets_at": 0.0}},
                "credits": false, "read_at": t, "requested_at": requested
            })
            .to_string(),
        )
        .unwrap();
        tend(&cfg);
        assert!(out(&cfg).is_some());
        assert_eq!(told(&d).len(), 1);
    }
}
