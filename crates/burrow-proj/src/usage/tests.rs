// What a refused run looks like, as Claude Code wrote it on 2026-11-02 (its
// stream, cut down to what's read), and the machine's state from it: out
// until the reset, the owner told once, and back with the next run that
// isn't refused.

use std::fs;
use std::os::unix::fs::PermissionsExt;

use serde_json::{Value, json};

use super::*;
use crate::config::tests::example;

const ASSISTANT: &str = r#"{"type":"assistant","message":{"model":"<synthetic>","role":"assistant","content":[{"type":"text","text":"You've hit your monthly spend limit · raise it at https://claude.ai/settings/usage?from=cc_cli_limit_message · your weekly limit resets Nov 4, 12pm (America/Los_Angeles)"}]},"session_id":"34fa7061","error":"rate_limit","is_api_error_message":true,"api_error_status":429,"api_error":"usage_limit_reached","api_error_params":{"rate_limit_info":{"status":"rejected","resetsAt":1793822400,"rateLimitType":"seven_day","overageStatus":"rejected","isUsingOverage":false}}}"#;
const RESULT: &str = r#"{"type":"result","subtype":"success","is_error":true,"num_turns":1,"api_error_status":429,"api_error":"usage_limit_reached","result":"You've hit your monthly spend limit · raise it at https://claude.ai/settings/usage?from=cc_cli_limit_message · your weekly limit resets Nov 4, 12pm (America/Los_Angeles)","session_id":"34fa7061"}"#;
/// A five-hour window's allowance as every run's stream reports it: not a
/// refusal.
const ALLOWED: &str = r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","resetsAt":1792836000,"rateLimitType":"five_hour"}}"#;

/// 2026-11-02 02:47 UTC, when that refusal came.
const NOW: i64 = 1_793_587_665;

fn v(s: &str) -> Value {
    serde_json::from_str(s).unwrap()
}

#[test]
fn a_refused_run_says_when_its_limit_resets() {
    let stream = format!("{ALLOWED}\n{ASSISTANT}\n{RESULT}\n");
    let (until, said) = refused(&v(RESULT), &stream, NOW).unwrap();
    // Nov 4, 12pm PST.
    assert_eq!(until, Some(1_793_822_400));
    // Its words say so too, for a stream that has no timestamp.
    assert_eq!(refused(&v(RESULT), "", NOW).unwrap().0, Some(1_793_822_400));
    assert!(
        said.starts_with("You've hit your monthly spend limit"),
        "{said}"
    );
    // Claude Code's other limits, by their words alone, the older one with
    // its reset after a bar, one with its zone read by those words.
    for (said, until) in [
        (
            "Claude AI usage limit reached|1793822400",
            Some(1_793_822_400),
        ),
        ("5-hour limit reached ∙ resets 3pm", None),
        ("Opus weekly limit reached ∙ resets Mon 9am", None),
        ("You've reached your usage limit.", None),
        (
            "You've hit your limit · resets 3pm (America/Los_Angeles)",
            Some(1_793_660_400),
        ),
    ] {
        let r = json!({"type": "result", "is_error": true, "result": said});
        assert_eq!(
            refused(&r, "", NOW),
            Some((until, said.to_owned())),
            "{said}"
        );
    }
    // An error of another kind isn't one, nor is an answer that talks of a
    // limit, nor a run whose window was only reported.
    let boom = json!({"type": "result", "is_error": true, "errors": ["boom"]});
    assert_eq!(refused(&boom, ALLOWED, NOW), None);
    let answer = json!({"type": "result", "is_error": false, "result": "Usage limit reached on the API, so I added a retry."});
    assert_eq!(refused(&answer, "", NOW), None);
    let api = json!({"type": "result", "is_error": true, "result": "API Error: 529 overloaded"});
    assert_eq!(refused(&api, "", NOW), None);
}

/// proj's config with its jobs in a dir of its own, the watchdog's waits
/// beside them, and a notify that keeps what it's asked, a line each, and
/// says a notice may still be open while there's a `pending` file.
fn rig() -> (tempfile::TempDir, Config) {
    let d = tempfile::tempdir().unwrap();
    let notify = d.path().join("notify");
    let dir = d.path().display();
    fs::write(
        &notify,
        format!(
            "#!/bin/sh\nif [ \"$1\" = pending ]; then [ -e {dir}/pending ]; exit; fi\necho \"$1 $2 $3\" >>{dir}/told\n"
        ),
    )
    .unwrap();
    fs::set_permissions(&notify, fs::Permissions::from_mode(0o755)).unwrap();
    let mut cfg = example();
    cfg["jobs"] = json!(d.path());
    cfg["notify"] = json!(notify);
    cfg["waits"] = json!(d.path().join(".waits.json"));
    (d, serde_json::from_value(cfg).unwrap())
}

fn told(d: &tempfile::TempDir) -> Vec<String> {
    fs::read_to_string(d.path().join("told"))
        .unwrap_or_default()
        .lines()
        .map(String::from)
        .collect()
}

const SAID: &str = "You've hit your monthly spend limit";

#[test]
fn out_of_usage_till_it_resets_and_told_once() {
    let (d, cfg) = rig();
    let t = now();
    assert_eq!(out(&cfg), None);
    let s = record(&cfg, "web-1", Some(t + 7200), SAID, t);
    assert_eq!(
        (s.until, s.at, s.job.as_str(), s.back),
        (t + 7200, t, "web-1", None)
    );
    assert_eq!(out(&cfg), Some(s.clone()));
    let open = told(&d);
    assert_eq!(open.len(), 1);
    assert!(
        open[0].starts_with("open proj-usage Claude is out of usage until "),
        "{open:?}"
    );
    // Every turn running is refused too: told once, and out till the latest
    // reset any of them gives, from the first refusal.
    assert_eq!(
        record(&cfg, "web-2", Some(t + 3600), SAID, t + 5).until,
        t + 7200
    );
    let s3 = record(&cfg, "web-3", Some(t + 9000), SAID, t + 6);
    assert_eq!((s3.until, s3.at, s3.job.as_str()), (t + 9000, t, "web-1"));
    assert_eq!(told(&d).len(), 1);
    // Told again only once it's been RETELL: GitHub may not have taken it.
    remind(&cfg);
    assert_eq!(told(&d).len(), 1);
    let mut old = kept(&cfg).unwrap();
    old.told = t - RETELL;
    keep(&cfg, &old);
    remind(&cfg);
    assert_eq!(told(&d).len(), 2);
    assert!(told(&d)[1].starts_with("open proj-usage"));
    // A reset unsaid, or already past: an hour on.
    back(&cfg, None);
    fs::remove_file(cfg.jobs.join(FILE)).unwrap();
    assert_eq!(record(&cfg, "web-4", None, SAID, t).until, t + UNSAID);
    back(&cfg, None);
    assert_eq!(
        record(&cfg, "web-4", Some(t - 5), SAID, t).until,
        t + UNSAID
    );
    // Past its reset it's no longer out, though no run has said so yet.
    let mut past = kept(&cfg).unwrap();
    past.until = t - 1;
    keep(&cfg, &past);
    assert_eq!(out(&cfg), None);
}

/// A run that started before the refusal proves nothing: its last call may
/// have come before it. One that started after does, and so does `proj
/// usage back`; a refusal after that is a hold of its own, told again.
#[test]
fn back_with_a_run_that_started_after_it() {
    let (d, cfg) = rig();
    let t = now();
    record(&cfg, "web-1", Some(t + 7200), SAID, t);
    back(&cfg, Some(t - 30));
    back(&cfg, Some(t));
    assert!(out(&cfg).is_some() && told(&d).len() == 1);
    back(&cfg, Some(t + 1));
    assert_eq!(out(&cfg), None);
    assert!(kept(&cfg).unwrap().back.is_some());
    assert_eq!(
        told(&d)[1..],
        ["close proj-usage Claude has usage again, and proj's jobs are running.".to_owned()]
    );
    let again = record(&cfg, "web-2", Some(t + 9000), SAID, t + 2);
    assert_eq!(
        (again.at, again.job.as_str(), again.back),
        (t + 2, "web-2", None)
    );
    assert_eq!(told(&d).len(), 3);
    back(&cfg, None);
    assert_eq!(out(&cfg), None);
    assert_eq!(told(&d).len(), 4);
}

/// The close is sent again until HOST-notify says it got through, and the
/// record stays a stall's time after the release (the watchdog's deadlines
/// for turns in line count from it), then goes.
#[test]
fn the_close_until_it_got_through_and_the_record_a_stall_after() {
    let (d, cfg) = rig();
    let t = now();
    record(&cfg, "web-1", Some(t + 7200), SAID, t);
    fs::write(d.path().join("pending"), "").unwrap();
    back(&cfg, None);
    assert_eq!(told(&d).len(), 2);
    let release = kept(&cfg).unwrap().release();
    assert!(release >= t && release <= now());
    // Still pending, but sent just now: not again yet; RETELL/12 on, again.
    back(&cfg, Some(t + 1));
    assert_eq!(told(&d).len(), 2);
    let mut s = kept(&cfg).unwrap();
    s.told -= RETELL / 12;
    keep(&cfg, &s);
    back(&cfg, Some(t + 1));
    assert_eq!(told(&d).len(), 3);
    assert!(told(&d)[2].starts_with("close proj-usage"));
    // Through, but within a stall of the release: kept.
    fs::remove_file(d.path().join("pending")).unwrap();
    back(&cfg, Some(t + 1));
    assert!(kept(&cfg).is_some());
    // A stall on: gone.
    let mut s = kept(&cfg).unwrap();
    s.back = Some(t - cfg.stall_queued() - 1);
    keep(&cfg, &s);
    back(&cfg, Some(t + 1));
    assert_eq!(kept(&cfg), None);
    assert_eq!(told(&d).len(), 3);
}

/// Every change of hold moves the deadline of every turn the watchdog
/// watches: each is looked at again (one whose job is gone, dropped).
#[test]
fn every_change_looks_at_every_watched_turn_again() {
    let (d, cfg) = rig();
    let waits = d.path().join(".waits");
    fs::create_dir_all(&waits).unwrap();
    for (when, change) in [
        (
            "record",
            Box::new(|c: &Config| {
                record(c, "web-1", Some(now() + 60), SAID, now());
            }) as Box<dyn Fn(&Config)>,
        ),
        ("back", Box::new(|c: &Config| back(c, None))),
    ] {
        fs::write(waits.join("gone-1"), "{}\n").unwrap();
        change(&cfg);
        assert!(!waits.join("gone-1").exists(), "{when}");
    }
}
