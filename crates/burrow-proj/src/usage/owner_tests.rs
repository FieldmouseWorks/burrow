//! The owner's pause lives in the settings overlay and changes through its door.
use super::tests::{rig, told};
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
    assert!(record(&cfg, "j", Some(now() + 3600), "refused", past - 1).is_err());
    assert!(out(&cfg).is_none());
    assert!(told(&d).last().unwrap().starts_with("close"));
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
