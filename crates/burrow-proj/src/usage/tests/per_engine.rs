//! Per-engine usage holds, expired-login notices, meter recovery, slot room,
//! snapshot reporting, and v2-only write cutover.

use std::fs;

use burrow_usage_reset::meter::{Snapshot as MeterSnapshot, Window};
use serde_json::{Value, json};

use super::*;
use crate::usage::login::Run;

#[test]
fn hold_or_login_expiry_on_engine_a_never_blocks_engine_b() {
    let (d, mut cfg) = rig();
    cfg.pipeline_slots = 0;
    cfg.min_free_memory = 0;
    let t = now();

    // Usage refusal on engine-a blocks engine-a (host + sandbox) and leaves engine-b free.
    let hold_a = record_for(
        &cfg,
        "engine-a",
        "job-a1",
        Some(t + 3600),
        "engine-a quota exhausted",
        t,
    )
    .unwrap();
    assert_eq!(hold_a.engine, "engine-a");
    assert!(out_for(&cfg, &Run::new("engine-a", false)).is_some());
    assert!(out_for(&cfg, &Run::new("engine-a", true)).is_some());
    assert!(out_for(&cfg, &Run::new("engine-b", false)).is_none());
    assert!(!crate::slot::seen_room_engine(&cfg, "engine-a", false));
    assert!(crate::slot::seen_room_engine(&cfg, "engine-b", false));
    assert_eq!(
        told(&d),
        [format!(
            "open proj-usage-engine-a engine-a is out of usage until {}",
            when(t + 3600)
        )]
    );

    // A successful run on engine-b must NOT clear engine-a's hold.
    answered_for(&cfg, "engine-b", t + 10, false);
    assert!(out_for(&cfg, &Run::new("engine-a", false)).is_some());

    // Expired-login refusal on engine-b holds only host runs of engine-b,
    // opens its own notice key, and lets sandboxed engine-b runs through.
    let login_b = record_login(&cfg, "engine-b", "job-b1", "auth token expired", t + 2).unwrap();
    assert!(login_b.is_login());
    assert!(out_for(&cfg, &Run::new("engine-b", false)).is_some());
    assert!(out_for(&cfg, &Run::new("engine-b", true)).is_none());
    assert!(!crate::slot::seen_room_engine(&cfg, "engine-b", false));
    assert!(crate::slot::seen_room_engine(&cfg, "engine-b", true));
    assert!(crate::slot::seen_room_engine(&cfg, "engine-c", false));
    assert_eq!(
        told(&d)[1],
        "open proj-usage-engine-b engine-b's login expired: log in again on this machine"
    );

    // Answering on engine-a closes only engine-a's notice and leaves engine-b held.
    answered_for(&cfg, "engine-a", t + 20, false);
    assert!(out_for(&cfg, &Run::new("engine-a", false)).is_none());
    assert!(out_for(&cfg, &Run::new("engine-b", false)).is_some());
    assert_eq!(
        told(&d)[2],
        "close proj-usage-engine-a engine-a has usage again, and proj's jobs are running."
    );

    // Answering on host engine-b closes engine-b's login notice.
    answered_for(&cfg, "engine-b", t + 25, false);
    assert!(out_for(&cfg, &Run::new("engine-b", false)).is_none());
    assert_eq!(
        told(&d)[3],
        "close proj-usage-engine-b engine-b is logged in again, and proj's jobs are running."
    );
}

#[test]
fn per_engine_and_global_owner_pauses_and_targeted_back() {
    let (_d, mut cfg) = rig();
    cfg.pipeline_slots = 0;
    let t = now();

    // Per-engine owner pause holds only engine-a.
    let args = [
        "usage",
        "hold",
        &(t + 3600).to_string(),
        "--engine",
        "engine-a",
        "--reason",
        "maintenance on engine-a",
    ]
    .map(String::from);
    crate::cmd::dispatch(&cfg, &args).unwrap();
    assert!(out_for(&cfg, &Run::new("engine-a", false)).is_some());
    assert!(out_for(&cfg, &Run::new("engine-b", false)).is_none());
    assert!(!crate::slot::seen_room_engine(&cfg, "engine-a", false));
    assert!(crate::slot::seen_room_engine(&cfg, "engine-b", false));

    // Also hold engine-b via refusal.
    record_for(&cfg, "engine-b", "jb", Some(t + 3600), "quota hit", t).unwrap();
    assert!(out_for(&cfg, &Run::new("engine-b", false)).is_some());

    // `proj usage back --engine engine-a` releases only engine-a.
    let back_a = ["usage", "back", "--engine", "engine-a"].map(String::from);
    crate::cmd::dispatch(&cfg, &back_a).unwrap();
    assert!(out_for(&cfg, &Run::new("engine-a", false)).is_none());
    assert!(out_for(&cfg, &Run::new("engine-b", false)).is_some());

    // Global owner pause holds every engine.
    hold(&cfg, t + 7200, "all paused").unwrap();
    assert!(out_for(&cfg, &Run::new("engine-a", false)).unwrap().owner);
    assert!(out_for(&cfg, &Run::new("engine-b", false)).unwrap().owner);
    assert!(out_for(&cfg, &Run::new("engine-c", true)).unwrap().owner);

    // `proj usage back` releases everything.
    back(&cfg, None);
    assert!(all_out(&cfg).is_empty());
}

#[test]
fn fresh_meter_releases_only_its_own_engine_hold() {
    let (d, mut cfg) = rig();
    cfg.meters = Some(d.path().join("meters"));
    let t = now();

    record_for(&cfg, "engine-a", "ja", Some(t + 3600), "spent a", t - 10).unwrap();
    record_for(&cfg, "engine-b", "jb", Some(t + 3600), "spent b", t - 10).unwrap();

    let meter_a = MeterSnapshot {
        windows: [(
            "daily".into(),
            Window {
                used: 15.0,
                resets_at: (t + 7200) as f64,
            },
        )]
        .into(),
        credits: false,
        read_at: t as f64,
        requested_at: Some(t as f64),
    };
    meter_a
        .record(cfg.meters.as_ref().unwrap(), "engine-a")
        .unwrap();

    // Read-only metered view sees engine-a recovered and engine-b still held.
    assert!(out_metered_for(&cfg, &Run::new("engine-a", false)).is_none());
    assert!(out_metered_for(&cfg, &Run::new("engine-b", false)).is_some());

    // Scheduling release clears only engine-a.
    metered(&cfg);
    assert!(out_for(&cfg, &Run::new("engine-a", false)).is_none());
    assert!(out_for(&cfg, &Run::new("engine-b", false)).is_some());
}

#[test]
fn legacy_files_read_cleanly_and_upgrade_to_v2_on_write() {
    let (_d, cfg) = rig();
    let t = now();

    // Seed legacy flat `.usage.json`, legacy scalar `.usage.released`, and legacy tab `.usage.admission`.
    fs::write(
        cfg.jobs.join(FILE),
        json!({
            "until": t + 3600,
            "said": "You've hit your monthly spend limit",
            "job": "old-job",
            "at": t - 30,
            "told": t - 30
        })
        .to_string(),
    )
    .unwrap();
    fs::write(cfg.jobs.join(RELEASED), format!("{}\n", t - 100)).unwrap();
    fs::write(
        cfg.jobs.join(".usage.admission"),
        format!(
            "{{\"until\":{},\"cutoff\":{},\"committed\":true}}\t{}\n",
            t - 50,
            (t - 50) * 1_000_000_000,
            (t - 50) * 1_000_000_000
        ),
    )
    .unwrap();

    // Legacy reads work seamlessly and map to `claude`.
    let claude_hold = out_for(&cfg, &Run::new("claude", false)).unwrap();
    assert_eq!(claude_hold.engine, "claude");
    assert_eq!(claude_hold.said, "Claude reached its extra-usage cap");
    assert!(out_for(&cfg, &Run::new("engine-b", false)).is_none());
    assert_eq!(last_release_for(&cfg, "claude"), Some(t - 100));

    // Writing a new hold on engine-b preserves claude's hold and writes ONLY v2 format.
    record_for(&cfg, "engine-b", "job-b", Some(t + 1800), "quota b", t).unwrap();
    let disk: Value = serde_json::from_slice(&fs::read(cfg.jobs.join(FILE)).unwrap()).unwrap();
    assert_eq!(disk["version"], 2);
    assert!(
        disk.get("until").is_none(),
        "flat legacy hold must not be written"
    );
    assert!(disk["holds"]["claude"]["until"].as_i64() == Some(t + 3600));
    assert!(disk["holds"]["engine-b"]["until"].as_i64() == Some(t + 1800));
}

#[test]
fn snapshot_exposes_all_engines_meters_and_holds() {
    let (d, mut cfg) = rig();
    cfg.meters = Some(d.path().join("meters"));
    cfg.projects = d.path().join("src");
    cfg.inbox = None;
    let t = now();

    fs::create_dir_all(cfg.meters.as_ref().unwrap()).unwrap();
    MeterSnapshot {
        windows: [(
            "seven_day".into(),
            Window {
                used: 20.0,
                resets_at: (t + 7200) as f64,
            },
        )]
        .into(),
        credits: false,
        read_at: t as f64,
        requested_at: Some(t as f64),
    }
    .record(cfg.meters.as_ref().unwrap(), "antigravity")
    .unwrap();

    record_for(
        &cfg,
        "antigravity",
        "ag-1",
        Some(t + 3600),
        "quota reached",
        t,
    )
    .unwrap();

    let snap = crate::snapshot::Snapshot::take(&cfg, 5).unwrap();
    let ag = snap
        .meters
        .iter()
        .find_map(|m| match m {
            crate::snapshot::Meter::Engine(e) if e.engine == "antigravity" => Some(e),
            _ => None,
        })
        .expect("antigravity meter present in snapshot");
    assert!(ag.reading.is_some());
    assert_eq!(ag.hold.as_ref().unwrap().reason, "quota reached");
    let claude = snap
        .meters
        .iter()
        .find_map(|m| match m {
            crate::snapshot::Meter::Engine(e) if e.engine == "claude" => Some(e),
            _ => None,
        })
        .expect("claude meter present in snapshot");
    assert!(claude.hold.is_none());
    let status = snap.status();
    assert!(
        status.contains("antigravity is out of usage until "),
        "{status}"
    );
    assert!(!status.contains("Claude is out of usage"), "{status}");
}
