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
        cfg.jobs.join(LEGACY_FILE),
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
    fs::write(cfg.jobs.join(LEGACY_RELEASED), format!("{}\n", t - 100)).unwrap();
    fs::write(
        cfg.jobs.join(LEGACY_ADMISSION),
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

    // Writing a new hold on engine-b preserves claude's hold and writes v2 to `.usage-holds.json`
    // while keeping `.usage.json` in strict legacy format for retained deploy turns.
    record_for(&cfg, "engine-b", "job-b", Some(t + 1800), "quota b", t).unwrap();
    let disk: Value = serde_json::from_slice(&fs::read(cfg.jobs.join(FILE)).unwrap()).unwrap();
    assert_eq!(disk["version"], 2);
    assert!(
        disk.get("until").is_none(),
        "v2 store must hold per-engine map"
    );
    assert!(disk["holds"]["claude"]["until"].as_i64() == Some(t + 3600));
    assert!(disk["holds"]["engine-b"]["until"].as_i64() == Some(t + 1800));
    let legacy: PreChangeSpent =
        serde_json::from_slice(&fs::read(cfg.jobs.join(LEGACY_FILE)).unwrap()).unwrap();
    assert_eq!(legacy.until, t + 3600);
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

/// Exact strict on-disk schema parsed by retained pre-change `proj` turns during a deploy.
#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct PreChangeSpent {
    until: i64,
    said: String,
    job: String,
    at: i64,
    #[serde(default)]
    back: Option<i64>,
    #[serde(default)]
    told: i64,
    #[serde(default)]
    last: Option<i64>,
}

#[test]
fn retained_legacy_process_sees_holds_and_persists_refusals_during_deploy() {
    let (_d, cfg) = rig();
    let t = now();
    let legacy_path = cfg.jobs.join(".usage.json");
    let legacy_released = cfg.jobs.join(".usage.released");

    // 1. New binary sets a hold on `claude` and a separate hold on `engine-b`.
    record_for(&cfg, "claude", "new-cl", Some(t + 1800), SAID, t).unwrap();
    record_for(&cfg, "engine-b", "new-b", Some(t + 2400), "quota b", t).unwrap();

    // A retained pre-change `proj` binary reading `.usage.json` must parse it
    // cleanly with strict `PreChangeSpent` and observe the active Claude hold.
    let old: PreChangeSpent = serde_json::from_slice(&fs::read(&legacy_path).unwrap())
        .expect("retained pre-change binary must parse .usage.json without error");
    assert!(old.back.is_none() && old.until == t + 1800);

    // 2. The retained pre-change binary hits a later refusal and updates `.usage.json`.
    let mut updated = old;
    updated.until = t + 3600;
    updated.last = Some(t + 5);
    updated.job = "old-turn".into();
    updated.said = "Claude reached its weekly allowance".into();
    fs::write(&legacy_path, serde_json::to_string(&updated).unwrap()).unwrap();

    // New binary must see the old binary's extended refusal without losing `engine-b`.
    let cl = out_for(&cfg, &Run::new("claude", false)).unwrap();
    assert_eq!(cl.until, t + 3600);
    assert_eq!(cl.last, Some(t + 5));
    assert_eq!(
        out_for(&cfg, &Run::new("engine-b", false)).unwrap().until,
        t + 2400
    );

    // 3. New binary releases `claude` (leaving only `engine-b` held).
    back_for(&cfg, Some("claude"), None);
    assert!(out_for(&cfg, &Run::new("claude", false)).is_none());
    assert!(out_for(&cfg, &Run::new("engine-b", false)).is_some());
    // `.usage.released` must remain a plain integer so retained old binaries parse it.
    let released_ts: i64 = fs::read_to_string(&legacy_released)
        .unwrap()
        .trim()
        .parse()
        .expect("retained pre-change binary must parse .usage.released as i64");
    assert!(released_ts >= t);
    // `.usage.json` reflects the released Claude hold (never blocking old turns).
    let after_release: PreChangeSpent = serde_json::from_slice(&fs::read(&legacy_path).unwrap())
        .expect("released .usage.json must stay valid for old binary");
    assert!(after_release.back.is_some());

    // 4. Retained old binary hits a brand-new refusal after release while `engine-b` is still held.
    let fresh = PreChangeSpent {
        until: t + 5400,
        said: "Claude reached its weekly allowance".into(),
        job: "old-after-release".into(),
        at: t + 10,
        back: None,
        told: t + 10,
        last: Some(t + 10),
    };
    fs::write(&legacy_path, serde_json::to_string(&fresh).unwrap()).unwrap();
    let cl2 = out_for(&cfg, &Run::new("claude", false)).unwrap();
    assert_eq!(cl2.until, t + 5400);
    assert_eq!(cl2.job, "old-after-release");
    assert_eq!(
        out_for(&cfg, &Run::new("engine-b", false)).unwrap().until,
        t + 2400
    );

    // 5. New binary sets a global `*` owner pause: old binary sees it held and its
    // `last` fence prevents a pre-deadline successful old turn from releasing it.
    hold_for(&cfg, ALL_ENGINES, t + 7200, "deploy maintenance").unwrap();
    let owner_legacy: PreChangeSpent = serde_json::from_slice(&fs::read(&legacy_path).unwrap())
        .expect("owner pause must be readable by retained old binary");
    assert!(owner_legacy.back.is_none());
    assert_eq!(owner_legacy.until, t + 7200);
    assert!(
        owner_legacy.last.unwrap_or(owner_legacy.at) >= t + 7199,
        "owner pause mirror must fence early back() from retained old turns"
    );
}

/// Exact pre-change `read()` + `holds_run()` semantics from `origin/main`:
/// sets `owner = true` only when `.usage.json`'s `job` references a valid
/// `.usage-owner-*` metadata file whose deadline is past `refused()`.
fn pre_change_read(cfg: &Config) -> Option<(PreChangeSpent, bool)> {
    let raw: PreChangeSpent =
        serde_json::from_slice(&fs::read(cfg.jobs.join(".usage.json")).ok()?).unwrap();
    let refused = raw.last.unwrap_or(raw.at);
    if raw
        .job
        .strip_prefix(".usage-owner-")
        .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric()))
    {
        let mut meta: PreChangeSpent =
            serde_json::from_slice(&fs::read(cfg.jobs.join(&raw.job)).unwrap()).unwrap();
        if refused < meta.until {
            meta.told = raw.told;
            meta.back = raw.back;
            return Some((meta, true));
        }
    }
    Some((raw, false))
}

fn pre_change_holds_run(spent: &PreChangeSpent, owner: bool, api: bool, at: i64) -> bool {
    spent.back.is_none() && spent.until > at && (owner || !api)
}

#[test]
fn retained_legacy_api_turn_honors_mirrored_global_owner_pause_but_not_claude_only_pause() {
    let (_d, cfg) = rig();
    let t = now();

    // Global `*` owner pause: pre-change reader must see `owner == true` and
    // block BOTH `claude` and `claude-api` runs.
    hold_for(&cfg, ALL_ENGINES, t + 7200, "global maintenance").unwrap();
    let (spent, owner) =
        pre_change_read(&cfg).expect("global owner pause must be readable by pre-change binary");
    assert!(
        owner,
        "pre-change reader must recognize mirrored global `*` pause as owner=true"
    );
    assert_eq!(spent.said, "global maintenance");
    assert!(pre_change_holds_run(&spent, owner, false, t));
    assert!(
        pre_change_holds_run(&spent, owner, true, t),
        "pre-change claude-api run must be blocked by mirrored global `*` owner pause"
    );

    // Release global pause -> pre-change reader sees hold ended for both engines.
    back_for(&cfg, Some(ALL_ENGINES), None);
    let (released, released_owner) = pre_change_read(&cfg).unwrap();
    assert!(!pre_change_holds_run(&released, released_owner, false, t));
    assert!(!pre_change_holds_run(&released, released_owner, true, t));

    // Claude-only owner pause: pre-change reader blocks `claude`, NOT `claude-api`.
    hold_for(&cfg, "claude", t + 5400, "claude only pause").unwrap();
    let (cl_spent, cl_owner) = pre_change_read(&cfg).unwrap();
    assert!(
        !cl_owner,
        "claude-only pause must not set global owner=true in legacy mirror"
    );
    assert!(pre_change_holds_run(&cl_spent, cl_owner, false, t));
    assert!(
        !pre_change_holds_run(&cl_spent, cl_owner, true, t),
        "pre-change claude-api run must stay free during a claude-only pause"
    );
}
