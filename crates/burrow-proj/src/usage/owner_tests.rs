//! The owner's pause lives in the settings overlay and changes through its door.
use super::tests::{cutoff, rig, told};
use super::*;
use std::fs;

fn dispatch(cfg: &Config, words: &[&str]) -> Result<(), crate::Fail> {
    crate::cmd::dispatch(
        cfg,
        &words.iter().map(|w| w.to_string()).collect::<Vec<_>>(),
    )
}

fn log(cfg: &Config) -> String {
    let overlay = &cfg.settings.as_ref().unwrap().overlay;
    fs::read_to_string(overlay.with_extension("log")).unwrap_or_default()
}

#[test]
fn a_hold_set_through_the_door_stops_turns_and_back_releases_it_and_both_are_logged() {
    let (d, cfg) = rig();
    let until = (now() + 7200).to_string();
    dispatch(&cfg, &["usage", "hold", &until, "--reason", "save usage"]).unwrap();
    let held = out(&cfg).expect("the next Claude turn finds the machine out");
    assert!(held.owner);
    assert_eq!(held.said, "save usage");
    assert_eq!(held.until.to_string(), until);
    assert!(
        !cfg.jobs.join(FILE).exists(),
        "nothing is kept in the old file"
    );
    assert_eq!(told(&d).len(), 1);
    // A successful run, or a refusal, neither ends nor replaces it.
    back(&cfg, Some(now() + 1));
    record(&cfg, "j", Some(now() + 99999), "refused", now()).unwrap();
    assert_eq!(out(&cfg), Some(held));
    fs::write(d.path().join("pending"), "").unwrap();
    dispatch(&cfg, &["usage", "back"]).unwrap();
    assert!(out(&cfg).is_none());
    assert!(
        told(&d).last().unwrap().starts_with("close"),
        "{:?}",
        told(&d)
    );
    let log = log(&cfg);
    assert!(
        log.contains("proj usage hold") && log.contains("usage-hold") && log.contains("save usage"),
        "{log}"
    );
    assert!(
        log.contains("proj usage back") && log.contains("usage-back"),
        "{log}"
    );
}

#[test]
fn a_hold_works_without_the_inbox_or_any_daemon_and_rejects_bad_arguments() {
    let (_d, cfg) = rig();
    for words in [
        vec!["usage", "hold", "0"],
        vec!["usage", "hold", "9999999999", "--reason"],
        vec!["usage", "hold", "9999999999", "--reason", ""],
        vec!["usage", "hold", "9999999999", "--unknown"],
    ] {
        assert!(dispatch(&cfg, &words).is_err());
        assert!(out(&cfg).is_none());
    }
    hold(&cfg, now() + 60, "now").unwrap();
    assert!(out(&cfg).is_some());
}

#[test]
fn an_expired_hold_closes_itself_and_a_refusal_before_it_ends_is_ignored() {
    let (d, cfg) = rig();
    hold(&cfg, now() + 60, "short").unwrap();
    let overlay = cfg.settings.as_ref().unwrap().overlay.clone();
    let past = now() - 5;
    fs::write(
        &overlay,
        format!(
            r#"{{"usageHold":{{"until":{past},"reason":"short","since":{}}}}}"#,
            past - 60
        ),
    )
    .unwrap();
    assert!(out(&cfg).is_none(), "it expires without a daemon");
    fs::write(d.path().join("pending"), "").unwrap();
    let stale = record(&cfg, "j", Some(now() + 3600), "refused", past - 1).unwrap();
    assert!(
        !stale.holds(now()),
        "a stale refusal waits as ended, not failed"
    );
    assert!(out(&cfg).is_none());
    assert!(
        !cfg.settings
            .as_ref()
            .unwrap()
            .overlay
            .to_string_lossy()
            .is_empty()
    );
    tend(&cfg);
    assert!(told(&d).last().unwrap().starts_with("close"));
    let again = record(&cfg, "j", Some(now() + 3600), "refused", past - 2).unwrap();
    assert!(
        !again.holds(now()),
        "still stale once the overlay is cleared"
    );
    assert!(log(&cfg).contains("deadline passed"));
    record(&cfg, "j", Some(now() + 3600), "refused", now()).unwrap();
    assert!(
        !out(&cfg).unwrap().owner,
        "a later refusal holds as an ordinary one"
    );
}

#[test]
fn a_hold_in_the_old_file_is_migrated_once() {
    let (_d, cfg) = rig();
    let until = now() + 3600;
    let at = now() - 10;
    let meta = ".usage-owner-abc123";
    let spent = |job: &str, last: i64| {
        json!({"until": until, "said": "old pause", "job": job, "at": at, "told": at, "last": last})
            .to_string()
    };
    fs::write(cfg.jobs.join(meta), spent(meta, at)).unwrap();
    fs::write(cfg.jobs.join(FILE), spent(meta, until - 1)).unwrap();
    tend(&cfg);
    let held = out(&cfg).unwrap();
    assert!(held.owner);
    assert_eq!((held.until, held.said.as_str()), (until, "old pause"));
    assert!(!cfg.jobs.join(FILE).exists() && !cfg.jobs.join(meta).exists());
    assert!(log(&cfg).contains("migrated"));
    let before = log(&cfg);
    tend(&cfg);
    assert_eq!(log(&cfg), before, "once");
    assert_eq!(out(&cfg), Some(held));
}

use serde_json::json;

#[test]
fn expiry_and_migration_leave_a_newer_overlay_decision_alone() {
    let (_d, cfg) = rig();
    let door = cfg.settings.as_ref().unwrap();
    let past = now() - 5;
    fs::write(
        &door.overlay,
        format!(
            r#"{{"usageHold":{{"until":{past},"reason":"old","since":{}}}}}"#,
            past - 60
        ),
    )
    .unwrap();
    // A replacement made after expiry was read stands against its cleanup.
    let fresh = (now() + 600).to_string();
    door.command(&["usage-hold".into(), fresh.clone(), "new".into()], "t")
        .unwrap();
    door.command(&["usage-expire".into(), past.to_string()], "t")
        .unwrap();
    assert_eq!(out(&cfg).unwrap().said, "new");
    // And a migration doesn't overwrite it.
    door.command(
        &[
            "usage-migrate".into(),
            (now() + 900).to_string(),
            "stale".into(),
        ],
        "t",
    )
    .unwrap();
    assert_eq!(out(&cfg).unwrap().said, "new");
}

#[test]
fn the_issue_door_takes_a_hold_with_a_spaced_reason_and_the_release() {
    let words = |s: &str| s.split_whitespace().map(String::from).collect::<Vec<_>>();
    let until = now() + 600;
    assert!(burrow_settings::valid_command(&words(&format!(
        "usage-hold {until} save my usage"
    ))));
    assert!(!burrow_settings::valid_command(&words(&format!(
        "usage-hold {until}"
    ))));
    assert!(!burrow_settings::valid_command(&words("usage-hold 1 late")));
    assert!(burrow_settings::valid_command(&words("usage-back")));
    let (_d, cfg) = rig();
    cfg.settings
        .as_ref()
        .unwrap()
        .command(
            &words(&format!("usage-hold {until} save my usage")),
            "issue",
        )
        .unwrap();
    assert_eq!(out(&cfg).unwrap().said, "save my usage");
}

#[test]
fn a_release_keeps_its_cutoff_over_an_older_refusal_and_works_without_settings() {
    let (_d, cfg) = rig();
    let t = now() - 100;
    record(&cfg, "web", Some(t + 10), "limit", t).unwrap();
    hold(&cfg, now() + 3600, "pause").unwrap();
    back(&cfg, None);
    let c = super::tests::cutoff(&cfg).unwrap();
    assert!(
        c > (t + 10) * 1_000_000_000,
        "the newer release bounds the queue"
    );
    let (_d2, mut bare) = rig();
    bare.settings = None;
    back(&bare, None);
}

#[test]
fn an_expired_old_file_hold_migrates_as_ended_and_pre_deadline_extensions_do_not_hold() {
    let (_d, cfg) = rig();
    let until = now() - 30;
    let meta = ".usage-owner-expired1";
    let body = |job: &str, mirror_until: i64| {
        json!({"until": mirror_until, "said": "old", "job": job, "at": until - 60, "told": 0, "last": until - 1})
            .to_string()
    };
    fs::write(cfg.jobs.join(meta), body(meta, until)).unwrap();
    // An old writer extended the mirror's deadline before the owner's.
    fs::write(cfg.jobs.join(FILE), body(meta, now() + 7200)).unwrap();
    tend(&cfg);
    assert!(out(&cfg).is_none());
    assert!(!cfg.jobs.join(FILE).exists() && !cfg.jobs.join(meta).exists());
    assert_eq!(last_release(&cfg), Some(until));
    assert!(cutoff(&cfg).is_some());
}

fn legacy(cfg: &Config, until: i64, at: i64) {
    let meta = ".usage-owner-abc123";
    let spent = |job: &str, last: i64| {
        json!({"until": until, "said": "old pause", "job": job, "at": at, "told": at, "last": last})
            .to_string()
    };
    fs::write(cfg.jobs.join(meta), spent(meta, at)).unwrap();
    fs::write(cfg.jobs.join(FILE), spent(meta, until - 1)).unwrap();
}

#[test]
fn an_older_pause_is_not_carried_over_after_a_release_through_any_door() {
    // A host or issue release with no hold on, then the old pause is found.
    let (_d, cfg) = rig();
    legacy(&cfg, now() + 3600, now() - 10);
    cfg.settings
        .as_ref()
        .unwrap()
        .command(&["usage-back".into()], "issue")
        .unwrap();
    tend(&cfg);
    tend(&cfg);
    assert!(out(&cfg).is_none(), "the release stands across retries");
    assert!(!cfg.jobs.join(FILE).exists());
    // proj's own release, with no hold in the overlay yet.
    let (_d, cfg) = rig();
    legacy(&cfg, now() + 3600, now() - 10);
    back(&cfg, None);
    tend(&cfg);
    assert!(out(&cfg).is_none());
    // A pause set after the release is a newer decision and is kept.
    legacy(&cfg, now() + 3600, now() + 1);
    tend(&cfg);
    assert!(out(&cfg).unwrap().owner);
}

#[test]
fn a_refusal_at_the_deadline_after_natural_expiry_is_kept_but_a_release_hides_its_second() {
    let (_d, cfg) = rig();
    let past = now() - 5;
    let overlay = cfg.settings.as_ref().unwrap().overlay.clone();
    let expired = |until: i64| {
        fs::write(
            &overlay,
            format!(r#"{{"usageHold":{{"until":{until},"reason":"x","since":0}}}}"#),
        )
        .unwrap();
    };
    expired(past);
    // Even a door release of the expired hold keeps it a natural expiry.
    cfg.settings
        .as_ref()
        .unwrap()
        .command(&["usage-back".into()], "issue")
        .unwrap();
    // At the deadline's second: genuine.
    earlier(&cfg, "a", now() + 7200, Some(past)).unwrap();
    assert!(out(&cfg).is_some(), "a refusal at the deadline holds");
    back(&cfg, None);
    // An explicit release hides its own second, expiries before it too.
    earlier(&cfg, "b", now() + 7200, Some(now())).unwrap();
    assert!(out(&cfg).is_none(), "a same-second release is inclusive");
}

#[test]
fn queued_jobs_keep_their_grace_after_an_owner_release_with_no_refusal_record() {
    let (_d, cfg) = rig();
    hold(&cfg, now() + 3600, "x").unwrap();
    assert!(completed(&cfg).unwrap().owner && completed(&cfg).unwrap().back.is_none());
    back(&cfg, None);
    assert!(kept(&cfg).is_none());
    let grace = completed(&cfg).expect("the release is the grace");
    assert_eq!(grace.back, Some(grace.release()));
    assert!(grace.release() >= now() - 2);
    assert_eq!(completed(&cfg).unwrap().release(), grace.release(), "fixed");
    // An older refusal does not shadow the later owner release.
    let t = now();
    record(&cfg, "j", Some(t + 100), "refused", t - 50).unwrap();
}

#[test]
fn the_notice_follows_a_replacement_and_an_expired_hold_closes_it_with_retries() {
    let (d, cfg) = rig();
    hold(&cfg, now() + 3600, "first").unwrap();
    assert_eq!(told(&d).len(), 1);
    // A replacement by the door while the notice is open is told by the look.
    cfg.settings
        .as_ref()
        .unwrap()
        .command(
            &[
                "usage-hold".into(),
                (now() + 7200).to_string(),
                "second".into(),
            ],
            "issue",
        )
        .unwrap();
    tend(&cfg);
    assert_eq!(told(&d).len(), 2, "{:?}", told(&d));
    tend(&cfg);
    assert_eq!(told(&d).len(), 2, "told once per hold, not hourly");
    // Its deadline passes with no refusal record; the close is chased.
    let overlay = cfg.settings.as_ref().unwrap().overlay.clone();
    let past = now() - 5;
    fs::write(
        &overlay,
        format!(r#"{{"usageHold":{{"until":{past},"reason":"x","since":0}}}}"#),
    )
    .unwrap();
    fs::write(d.path().join("pending"), "").unwrap();
    assert!(tend(&cfg).is_some(), "retried while unconfirmed");
    assert!(told(&d).last().unwrap().starts_with("close"));
    let n = told(&d).len();
    tend(&cfg);
    assert_eq!(told(&d).len(), n + 1, "sent again until confirmed");
    fs::remove_file(d.path().join("pending")).unwrap();
    tend(&cfg);
    let n = told(&d).len();
    assert_eq!(tend(&cfg), None);
    assert_eq!(told(&d).len(), n, "confirmed: nothing more");
    // An unreadable journal doesn't cancel it either.
    hold(&cfg, now() + 3600, "again").unwrap();
    fs::write(overlay.with_extension("usage-end"), "{bad").unwrap();
    assert!(tend(&cfg).is_some());
}
