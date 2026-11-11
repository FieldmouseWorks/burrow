//! Owner pauses: compatibility, deadline reconciliation and atomic generations.
use super::tests::{earlier_resets, rig, told};
use super::*;
use std::fs;

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
    back(&cfg, Some(now() + 1));
    assert!(kept(&cfg).unwrap().back.is_some());
    // A new refusal after expiry is an ordinary usage hold again.
    record(&cfg, "running", Some(now() + 7200), "refused", now()).unwrap();
    assert!(!out(&cfg).unwrap().owner);
}

// The disk schema used by turns that keep the previous binary through a deploy.
#[derive(Clone, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct LegacySpent {
    until: i64,
    said: String,
    job: String,
    at: i64,
    back: Option<i64>,
    told: i64,
    last: Option<i64>,
}

#[test]
fn owner_holds_survive_legacy_reads_refusals_and_successful_turns() {
    let (_d, cfg) = rig();
    record(&cfg, "ordinary", Some(now() + 60), "refused", now()).unwrap();
    let old: LegacySpent = serde_json::from_slice(&fs::read(cfg.jobs.join(FILE)).unwrap()).unwrap();
    assert!(old.back.is_none() && old.until > now());
    hold(&cfg, now() + 3600, "owner reason").unwrap();
    let held = out(&cfg).unwrap();
    let mut old: LegacySpent =
        serde_json::from_slice(&fs::read(cfg.jobs.join(FILE)).unwrap()).unwrap();
    // The old slot/retry loop still reads an active hold, and its successful
    // turn cannot satisfy back()'s started > last-refusal condition.
    assert!(old.back.is_none() && old.until > now());
    assert!(now() < old.last.unwrap());
    // Exactly the old put() update: a refusal extends the compatible mirror
    // and preserves the rest. It cannot erase the owner's actual deadline.
    old.until = old.until.max(now() + 7200);
    old.last = Some(old.last.unwrap().max(now()));
    fs::write(cfg.jobs.join(FILE), serde_json::to_vec(&old).unwrap()).unwrap();
    assert_eq!(out(&cfg), Some(held.clone()));
    back(&cfg, Some(now() + 1));
    assert_eq!(out(&cfg), Some(held));
    // Replacement owner pauses update both the old reader and the metadata.
    hold(&cfg, now() + 1200, "replacement").unwrap();
    let old: LegacySpent = serde_json::from_slice(&fs::read(cfg.jobs.join(FILE)).unwrap()).unwrap();
    assert_eq!(old.until, out(&cfg).unwrap().until);
    assert_eq!(out(&cfg).unwrap().said, "replacement");
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
fn legacy_refusals_at_and_after_expiry_survive_delayed_cleanup() {
    for delta in [-1, 0, 1] {
        let (_d, cfg) = rig();
        hold(&cfg, now() + 3600, "pause").unwrap();
        let mut expired = kept(&cfg).unwrap();
        let deadline = now() - 10;
        expired.until = deadline;
        keep(&cfg, &expired).unwrap();
        let mut old: LegacySpent =
            serde_json::from_slice(&fs::read(cfg.jobs.join(FILE)).unwrap()).unwrap();
        old.until = deadline + 7200;
        old.last = Some(old.last.unwrap().max(deadline + delta));
        fs::write(cfg.jobs.join(FILE), serde_json::to_vec(&old).unwrap()).unwrap();
        if delta < 0 {
            assert!(out(&cfg).is_none());
            assert!(kept(&cfg).unwrap().owner);
            tend(&cfg);
            assert!(kept(&cfg).unwrap().back.is_some());
        } else {
            let refused = out(&cfg).unwrap();
            assert!(!refused.owner);
            assert_eq!(refused.refused(), deadline + delta);
            tend(&cfg);
            assert_eq!(out(&cfg), Some(refused.clone()));
            // A stale successful turn cannot end the reconciled refusal.
            back(&cfg, Some(deadline + delta));
            assert_eq!(out(&cfg), Some(refused));
            earlier(&cfg, "import", deadline + 7300, Some(deadline + delta)).unwrap();
            adopt(&cfg).unwrap();
            tend(&cfg);
            assert!(!out(&cfg).unwrap().owner);
            assert!(out(&cfg).unwrap().until >= deadline + 7300);
            back(&cfg, Some(now() + 1));
            assert!(out(&cfg).is_none());
        }
    }
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
fn owner_generations_commit_only_when_the_shared_record_commits() {
    for seconds in [0, 1] {
        let (_d, cfg) = rig();
        hold(&cfg, now() + 3600, "original").unwrap();
        let original = out(&cfg).unwrap();
        let old_metadata = fs::read(cfg.jobs.join(&original.job)).unwrap();
        let mut replacement = original.clone();
        replacement.at += seconds;
        replacement.until += 1000;
        replacement.said = "replacement".into();
        // An interruption after preparing metadata has not committed anything.
        let prepared = owner_generation(&cfg, &replacement).unwrap();
        assert_eq!(out(&cfg), Some(original.clone()));
        assert_ne!(prepared.job, original.job);
        let blocked = cfg.jobs.join(format!("{FILE}.new"));
        fs::create_dir(&blocked).unwrap();
        assert!(keep(&cfg, &replacement).is_err());
        assert_eq!(out(&cfg), Some(original.clone()));
        assert_eq!(
            fs::read(cfg.jobs.join(&original.job)).unwrap(),
            old_metadata
        );
        fs::remove_dir(&blocked).unwrap();
        keep(&cfg, &replacement).unwrap();
        let committed = out(&cfg).unwrap();
        assert_eq!(committed.said, "replacement");
        assert_eq!(committed.until, replacement.until);
        assert_ne!(committed.job, original.job);
        // Concurrent readers of the previous record still have its metadata.
        assert_eq!(
            fs::read(cfg.jobs.join(&original.job)).unwrap(),
            old_metadata
        );
        let mut reminder = committed.clone();
        reminder.told += 1;
        fs::create_dir(&blocked).unwrap();
        assert!(keep(&cfg, &reminder).is_err());
        assert_eq!(out(&cfg), Some(committed.clone()));
        back(&cfg, None); // Failed release cannot downgrade the active owner.
        assert_eq!(out(&cfg), Some(committed.clone()));
        fs::remove_dir(&blocked).unwrap();
        keep(&cfg, &reminder).unwrap();
        assert_eq!(out(&cfg).unwrap().job, committed.job);
        back(&cfg, None);
        assert!(out(&cfg).is_none());
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
    for first in [
        "tend", "dated", "undated", "adopt", "current", "legacy", "hold",
    ] {
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
            "legacy" => {
                let mut raw: LegacySpent =
                    serde_json::from_slice(&fs::read(cfg.jobs.join(FILE)).unwrap()).unwrap();
                raw.until = reset;
                raw.last = Some(deadline);
                fs::write(cfg.jobs.join(FILE), serde_json::to_vec(&raw).unwrap()).unwrap();
                // Even the import, rather than tend, can first see this write.
                earlier(&cfg, "await", reset + 1000, Some(deadline - 1)).unwrap();
            }
            "hold" => {
                hold(&cfg, reset, "replacement").unwrap();
            }
            _ => unreachable!(),
        }
        let expected = out(&cfg);
        assert!(released(&cfg).unwrap() >= deadline - 1, "{first}");
        if matches!(first, "current" | "legacy" | "hold") {
            assert_eq!(expected.as_ref().unwrap().until, reset, "{first}");
        } else {
            assert!(expected.is_none(), "{first}");
        }
        // Every adoption route sees the durable cutoff, even after replacement.
        earlier(&cfg, "await", reset + 1000, Some(deadline - 1)).unwrap();
        earlier(&cfg, "watchdog", reset + 1000, None).unwrap();
        earlier_resets(&cfg, "owned-stale", reset + 1000, deadline - 1, true);
        earlier_resets(&cfg, "unowned-stale", reset + 1000, deadline - 1, false);
        adopt(&cfg).unwrap();
        tend(&cfg);
        assert_eq!(out(&cfg), expected, "{first}");
        if first != "hold" {
            // Cleanup cannot erase the cutoff; a refusal exactly at or after
            // expiry is still new and eligible, including through imports.
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
