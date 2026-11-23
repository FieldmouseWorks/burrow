//! Owner pauses: deadline reconciliation, atomic v2 writes, and backward-compatible
//! reading of legacy single-hold `.usage.json` and `.usage-owner-*` metadata.
use super::tests::{cutoff, earlier_resets, rig, told};
use super::*;
use serde_json::json;
use std::fs;

fn admission_cutoff(cfg: &Config) -> std::io::Result<Option<i64>> {
    let _held = super::lock(cfg).map_err(std::io::Error::other)?;
    super::admission_cutoff_locked(cfg)
}

#[test]
fn owner_hold_after_release_is_fresh_and_only_explicitly_released() {
    let (_d, cfg) = rig();
    back(&cfg, None);
    let args = [
        "usage",
        "hold",
        &(now() + 7200).to_string(),
        "--reason",
        "save usage",
    ]
    .map(String::from);
    crate::cmd::dispatch(&cfg, &args).unwrap();
    let held = out(&cfg).unwrap();
    assert!(held.owner);
    assert_eq!(held.engine, ALL_ENGINES);
    assert_eq!(held.said, "save usage");
    back(&cfg, Some(held.at + 1));
    assert_eq!(out(&cfg), Some(held));
    back(&cfg, None);
    assert!(out(&cfg).is_none());
    // Stale watchdog imports remain suppressed, unlike fresh owner holds.
    earlier(&cfg, "watchdog", now() + 7200, None).unwrap();
    assert!(out(&cfg).is_none());
    hold(&cfg, now() + 3600, "another pause").unwrap();
    assert!(out(&cfg).unwrap().owner);
}

#[test]
fn owner_hold_rejects_invalid_deadlines_and_arguments_without_changing_state() {
    let (_d, cfg) = rig();
    hold(&cfg, now() + 7200, "pause").unwrap();
    let before = kept(&cfg);
    for words in [
        vec!["usage", "hold", "0"],
        vec!["usage", "hold", "9999999999", "--reason"],
        vec!["usage", "hold", "9999999999", "--reason", ""],
        vec!["usage", "hold", "9999999999", "--unknown"],
    ] {
        assert!(
            crate::cmd::dispatch(
                &cfg,
                &words.into_iter().map(String::from).collect::<Vec<_>>()
            )
            .is_err()
        );
        assert_eq!(kept(&cfg), before);
    }
}

#[test]
fn owner_deadline_survives_refusals_and_expires_normally() {
    let (_d, cfg) = rig();
    hold(&cfg, now() + 60, "owner deadline").unwrap();
    let held = out(&cfg).unwrap();
    record(&cfg, "running", Some(now() + 7200), "refused", now()).unwrap();
    assert_eq!(out(&cfg), Some(held.clone()));
    let expired = Spent {
        until: now() - 1,
        ..held
    };
    keep(&cfg, &expired).unwrap();
    assert!(out(&cfg).is_none());
    tend(&cfg);
    assert!(kept(&cfg).unwrap().back.is_some());
    // A new refusal after expiry is an ordinary usage hold again.
    record(&cfg, "running", Some(now() + 7200), "refused", now()).unwrap();
    assert!(!out(&cfg).unwrap().owner);
}

#[test]
fn legacy_single_hold_and_owner_metadata_read_cleanly_while_writes_use_v2_only() {
    let (_d, cfg) = rig();
    let t = now();
    // 1. Legacy flat single-hold `.usage.json` reads as a `claude` hold.
    fs::write(
        cfg.jobs.join(FILE),
        json!({
            "until": t + 3600,
            "said": "You've hit your monthly spend limit",
            "job": "legacy-job",
            "at": t - 10,
            "back": null,
            "told": t - 10,
            "last": t - 10
        })
        .to_string(),
    )
    .unwrap();
    let loaded = out(&cfg).unwrap();
    assert_eq!(loaded.engine, DEFAULT_ENGINE);
    assert!(!loaded.owner);
    assert_eq!(loaded.job, "legacy-job");
    assert_eq!(loaded.said, "Claude reached its extra-usage cap");

    // 2. Legacy `.usage-owner-*` metadata reads as a global `*` owner hold.
    let owner_ref = ".usage-owner-legacy123";
    fs::write(
        cfg.jobs.join(owner_ref),
        json!({
            "until": t + 7200,
            "said": "legacy owner pause",
            "job": owner_ref,
            "at": t - 20,
            "back": null,
            "told": t - 20,
            "last": t - 20
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        cfg.jobs.join(FILE),
        json!({
            "until": t + 7200,
            "said": "legacy owner pause",
            "job": owner_ref,
            "at": t - 20,
            "back": null,
            "told": t - 20,
            "last": t + 7199
        })
        .to_string(),
    )
    .unwrap();
    let loaded_owner = out(&cfg).unwrap();
    assert!(loaded_owner.owner);
    assert_eq!(loaded_owner.engine, ALL_ENGINES);
    assert_eq!(loaded_owner.said, "legacy owner pause");

    // 3. New writes always use v2 per-engine JSON and create no `.usage-owner-*` sidecars.
    hold(&cfg, t + 1200, "v2 pause").unwrap();
    fs::remove_file(cfg.jobs.join(owner_ref)).unwrap();
    let raw: serde_json::Value =
        serde_json::from_slice(&fs::read(cfg.jobs.join(FILE)).unwrap()).unwrap();
    assert_eq!(raw["version"], 2);
    assert_eq!(raw["holds"]["*"]["owner"], true);
    assert_eq!(raw["holds"]["*"]["said"], "v2 pause");
    let has_owner_sidecar = fs::read_dir(&cfg.jobs)
        .unwrap()
        .filter_map(Result::ok)
        .any(|e| e.file_name().to_string_lossy().starts_with(".usage-owner-"));
    assert!(
        !has_owner_sidecar,
        "v2 write path must not create legacy owner sidecars"
    );
    back(&cfg, None);
    assert!(out(&cfg).is_none());
}

#[test]
fn owner_expiry_settles_without_a_claude_run_and_retries_its_close() {
    let (d, cfg) = rig();
    fs::write(d.path().join("pending"), "").unwrap();
    hold(&cfg, now() + 60, "short pause").unwrap();
    assert!(tend(&cfg).unwrap() <= out(&cfg).unwrap().until);
    let mut expired = kept(&cfg).unwrap();
    expired.until = now() - 1;
    keep(&cfg, &expired).unwrap();
    tend(&cfg);
    assert_eq!(kept(&cfg).unwrap().back, Some(expired.until));
    assert!(last_release(&cfg).unwrap() >= expired.until);
    assert!(out(&cfg).is_none());
    assert!(told(&d).iter().any(|s| s.starts_with("close ")));
    let closes = told(&d).iter().filter(|s| s.starts_with("close ")).count();
    let mut retry = kept(&cfg).unwrap();
    retry.told = now() - RETRY;
    keep(&cfg, &retry).unwrap();
    tend(&cfg);
    assert!(told(&d).iter().filter(|s| s.starts_with("close ")).count() > closes);
    fs::remove_file(d.path().join("pending")).unwrap();
    let mut settled = kept(&cfg).unwrap();
    settled.until = now() - cfg.stall_queued() - 2;
    settled.back = Some(settled.until);
    keep(&cfg, &settled).unwrap();
    tend(&cfg);
    assert!(kept(&cfg).is_none());
}

#[test]
fn current_refusals_respect_the_same_deadline_boundary() {
    for delta in [-1, 0, 1] {
        let (_d, cfg) = rig();
        hold(&cfg, now() + 3600, "pause").unwrap();
        let mut expired = kept(&cfg).unwrap();
        let deadline = now() - 10;
        expired.until = deadline;
        keep(&cfg, &expired).unwrap();
        let refusal = record(
            &cfg,
            "turn",
            Some(deadline + 7200),
            "refused",
            deadline + delta,
        )
        .unwrap();
        assert_eq!(refusal.owner, delta < 0);
        if delta >= 0 {
            tend(&cfg);
            assert_eq!(out(&cfg), Some(refusal));
        }
    }
}

#[test]
fn failed_creation_and_expiry_do_not_publish_partial_owner_state() {
    let (d, cfg) = rig();
    let blocked = cfg.jobs.join(format!("{FILE}.new"));
    fs::create_dir(&blocked).unwrap();
    assert!(hold(&cfg, now() + 3600, "not committed").is_err());
    assert!(kept(&cfg).is_none());
    fs::remove_dir(&blocked).unwrap();
    hold(&cfg, now() + 60, "expires").unwrap();
    let mut expired = kept(&cfg).unwrap();
    expired.until = now() - 1;
    keep(&cfg, &expired).unwrap();
    let before = kept(&cfg).unwrap();
    fs::create_dir(&blocked).unwrap();
    tend(&cfg);
    assert_eq!(kept(&cfg), Some(before));
    assert!(!told(&d).iter().any(|s| s.starts_with("close ")));
    fs::remove_dir(&blocked).unwrap();
    tend(&cfg);
    assert!(kept(&cfg).unwrap().back.is_some());
    assert!(told(&d).iter().any(|s| s.starts_with("close ")));
}

#[test]
fn expiry_cutoff_survives_every_first_writer_and_later_imports() {
    for first in ["tend", "dated", "undated", "adopt", "current", "hold"] {
        let (_d, cfg) = rig();
        hold(&cfg, now() + 3600, "expires").unwrap();
        let deadline = now() - 30;
        let mut expired = kept(&cfg).unwrap();
        expired.until = deadline;
        keep(&cfg, &expired).unwrap();
        let reset = now() + 3600;
        match first {
            "tend" => {
                tend(&cfg);
            }
            "dated" => {
                earlier(&cfg, "await", reset, Some(deadline - 1)).unwrap();
            }
            "undated" => {
                earlier(&cfg, "watchdog", reset, None).unwrap();
            }
            "adopt" => {
                earlier_resets(&cfg, "owned", reset, deadline - 1, true);
                earlier_resets(&cfg, "unowned", reset, deadline - 1, false);
                adopt(&cfg).unwrap();
            }
            "current" => {
                record(&cfg, "fresh", Some(reset), "refused", deadline).unwrap();
            }
            "hold" => {
                hold(&cfg, reset, "replacement").unwrap();
            }
            _ => unreachable!(),
        }
        let expected = out(&cfg);
        assert!(released(&cfg).unwrap() >= deadline - 1, "{first}");
        if matches!(first, "current" | "hold") {
            assert_eq!(expected.as_ref().unwrap().until, reset, "{first}");
        } else {
            assert!(expected.is_none(), "{first}");
        }
        earlier(&cfg, "await", reset + 1000, Some(deadline - 1)).unwrap();
        earlier(&cfg, "watchdog", reset + 1000, None).unwrap();
        earlier_resets(&cfg, "owned-stale", reset + 1000, deadline - 1, true);
        earlier_resets(&cfg, "unowned-stale", reset + 1000, deadline - 1, false);
        adopt(&cfg).unwrap();
        tend(&cfg);
        assert_eq!(out(&cfg), expected, "{first}");
        if first != "hold" {
            fs::remove_file(cfg.jobs.join(FILE)).unwrap();
            earlier(&cfg, "stale", reset + 1000, Some(deadline - 1)).unwrap();
            assert!(out(&cfg).is_none(), "{first}");
            earlier(&cfg, "boundary", reset + 2000, Some(deadline)).unwrap();
            assert_eq!(out(&cfg).unwrap().until, reset + 2000, "{first}");
            earlier(&cfg, "after", reset + 3000, Some(deadline + 1)).unwrap();
            assert_eq!(out(&cfg).unwrap().until, reset + 3000, "{first}");
        }
    }
}

#[test]
fn early_owner_release_is_not_later_replaced_by_its_original_deadline() {
    for writer in ["import", "refusal"] {
        let (_d, cfg) = rig();
        hold(&cfg, now() + 3600, "released early").unwrap();
        let deadline = now() - 30;
        let released_at = deadline - 60;
        let mut retired = kept(&cfg).unwrap();
        retired.until = deadline;
        keep(&cfg, &retired).unwrap();
        retired = kept(&cfg).unwrap();
        retired.back = Some(released_at);
        mark(&cfg, released_at).unwrap();
        keep(&cfg, &retired).unwrap();
        tend(&cfg);
        assert_eq!(released(&cfg).unwrap(), released_at);
        let reset = now() + 7200;
        if writer == "import" {
            earlier(&cfg, "after release", reset, Some(released_at + 1)).unwrap();
        } else {
            record(
                &cfg,
                "after release",
                Some(reset),
                "refused",
                released_at + 1,
            )
            .unwrap();
        }
        assert_eq!(out(&cfg).unwrap().until, reset, "{writer}");
        assert!(!out(&cfg).unwrap().owner, "{writer}");
    }
}

#[test]
fn fresh_meter_cannot_release_an_owner_pause() {
    let (d, mut cfg) = rig();
    cfg.meters = Some(d.path().join("meters"));
    hold(&cfg, now() + 7200, "owner pause").unwrap();
    let held = out(&cfg).unwrap();
    fs::create_dir_all(cfg.meters.as_ref().unwrap()).unwrap();
    fs::write(
        cfg.meters.as_ref().unwrap().join("claude.json"),
        serde_json::json!({
            "windows": {"seven_day": {"used": 1.0, "resets_at": 0.0}},
            "credits": false, "read_at": now() as f64, "requested_at": now() as f64
        })
        .to_string(),
    )
    .unwrap();
    metered(&cfg);
    tend(&cfg);
    assert_eq!(out(&cfg), Some(held));
    assert_eq!(last_release(&cfg), None);
}

#[test]
fn owner_release_preserves_precise_admission_boundary_through_cleanup() {
    let (_d, cfg) = rig();
    hold(&cfg, now() + 3600, "pause").unwrap();
    assert_eq!(cutoff(&cfg), None);
    let before = crate::now_nanos();
    back(&cfg, None);
    let after = crate::now_nanos();
    let boundary = cutoff(&cfg).unwrap();
    assert!((before..=after).contains(&boundary));
    let mut s = kept(&cfg).unwrap();
    assert_eq!(s.admission, Some(boundary));
    s.until = now() - cfg.stall_queued() - 2;
    s.back = Some(s.until);
    keep(&cfg, &s).unwrap();
    tend(&cfg);
    assert!(kept(&cfg).is_none());
    assert_eq!(cutoff(&cfg), Some(boundary));
}

#[test]
fn owner_expiry_retains_natural_admission_boundary() {
    let (_d, cfg) = rig();
    hold(&cfg, now() + 3600, "pause").unwrap();
    let mut s = kept(&cfg).unwrap();
    s.until = now() - 1;
    keep(&cfg, &s).unwrap();
    let boundary = s.until * 1_000_000_000;
    assert_eq!(cutoff(&cfg), Some(boundary));
    back(&cfg, Some(now() + 1));
    assert_eq!(cutoff(&cfg), Some(boundary));
}

#[test]
fn failed_release_journal_or_record_never_poisons_active_holds() {
    for owner in [false, true] {
        for boundary in [".usage.admission.new", ".usage.json.new"] {
            let (_d, cfg) = rig();
            let t = now();
            if owner {
                hold(&cfg, t + 3600, "pause").unwrap();
            } else {
                record(&cfg, "ordinary", Some(t + 3600), "refused", t - 10).unwrap();
            }
            fs::create_dir(cfg.jobs.join(boundary)).unwrap();
            back(&cfg, None);
            assert!(out(&cfg).is_some(), "{boundary}");
            assert_eq!(admission_cutoff(&cfg).unwrap(), None);
            fs::remove_dir(cfg.jobs.join(boundary)).unwrap();
            back(&cfg, None);
            let cutoff = admission_cutoff(&cfg).unwrap().unwrap();
            assert_eq!(kept(&cfg).unwrap().admission, Some(cutoff));
        }
    }
}

#[test]
fn legacy_releases_and_committed_cutoffs_read_cleanly() {
    let (_d, cfg) = rig();
    let t = now();
    // Legacy release without admission field includes the whole release second.
    fs::write(
        cfg.jobs.join(FILE),
        json!({
            "until": t + 3600,
            "said": "refused",
            "job": "ordinary",
            "at": t - 10,
            "back": t,
            "told": t
        })
        .to_string(),
    )
    .unwrap();
    assert_eq!(
        admission_cutoff(&cfg).unwrap(),
        Some((t + 1) * 1_000_000_000)
    );

    // Legacy release with ADMISSION_REF preserves exact nanosecond precision.
    let exact = t * 1_000_000_000 + 123_456_789;
    fs::write(
        cfg.jobs.join(FILE),
        json!({
            "until": t + 3600,
            "said": "refused",
            "job": format!(".usage-release-{exact}-ordinary"),
            "at": t - 10,
            "back": t,
            "told": t
        })
        .to_string(),
    )
    .unwrap();
    let loaded = kept(&cfg).unwrap();
    assert_eq!(loaded.job, "ordinary");
    assert_eq!(loaded.admission, Some(exact));
    assert_eq!(admission_cutoff(&cfg).unwrap(), Some(exact));
}

#[test]
fn committed_v2_record_recovers_precision_without_the_retention_journal() {
    for owner in [false, true] {
        let (_d, cfg) = rig();
        if owner {
            hold(&cfg, now() + 3600, "pause").unwrap();
        } else {
            record(&cfg, "ordinary", Some(now() + 3600), "refused", now() - 10).unwrap();
        }
        back(&cfg, None);
        let cutoff = admission_cutoff(&cfg).unwrap().unwrap();
        fs::remove_file(cfg.jobs.join(".usage.admission")).unwrap();
        let s = kept(&cfg).unwrap();
        assert_eq!(s.owner, owner);
        assert_eq!(s.admission, Some(cutoff));
        assert_eq!(admission_cutoff(&cfg).unwrap(), Some(cutoff));
        settle(&cfg, s.clone(), s.release() + cfg.stall_queued() + 1);
        assert!(kept(&cfg).is_none());
        assert_eq!(admission_cutoff(&cfg).unwrap(), Some(cutoff));
    }
}
