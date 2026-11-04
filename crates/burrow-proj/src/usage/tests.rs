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

fn v(s: &str) -> Value {
    serde_json::from_str(s).unwrap()
}

#[test]
fn a_refused_run_says_when_its_limit_resets() {
    let stream = format!("{ALLOWED}\n{ASSISTANT}\n{RESULT}\n");
    let (until, said) = refused(&v(RESULT), &stream).unwrap();
    // Nov 4, 12pm PST.
    assert_eq!(until, Some(1_793_822_400));
    assert!(
        said.starts_with("You've hit your monthly spend limit"),
        "{said}"
    );
    // Claude Code's other limits, by their words alone, the older one with
    // its reset after a bar.
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
            None,
        ),
    ] {
        let r = json!({"type": "result", "is_error": true, "result": said});
        assert_eq!(refused(&r, ""), Some((until, said.to_owned())), "{said}");
    }
    // An error of another kind isn't one, nor is an answer that talks of a
    // limit, nor a run whose window was only reported.
    let boom = json!({"type": "result", "is_error": true, "errors": ["boom"]});
    assert_eq!(refused(&boom, ALLOWED), None);
    let answer = json!({"type": "result", "is_error": false, "result": "Usage limit reached on the API, so I added a retry."});
    assert_eq!(refused(&answer, ""), None);
    let api = json!({"type": "result", "is_error": true, "result": "API Error: 529 overloaded"});
    assert_eq!(refused(&api, ""), None);
}

/// proj's config with its jobs in a dir of its own, and a notify that keeps
/// what it's asked, a line each.
fn rig() -> (tempfile::TempDir, Config) {
    let d = tempfile::tempdir().unwrap();
    let notify = d.path().join("notify");
    fs::write(
        &notify,
        format!(
            "#!/bin/sh\necho \"$1 $2 $3\" >>{}/told\n",
            d.path().display()
        ),
    )
    .unwrap();
    fs::set_permissions(&notify, fs::Permissions::from_mode(0o755)).unwrap();
    let mut cfg = example();
    cfg["jobs"] = json!(d.path());
    cfg["notify"] = json!(notify);
    (d, serde_json::from_value(cfg).unwrap())
}

fn told(d: &tempfile::TempDir) -> Vec<String> {
    fs::read_to_string(d.path().join("told"))
        .unwrap_or_default()
        .lines()
        .map(String::from)
        .collect()
}

#[test]
fn out_of_usage_till_it_resets_told_once_and_back_with_a_run() {
    let (d, cfg) = rig();
    let t = now();
    assert_eq!(out(&cfg), None);
    let s = record(
        &cfg,
        "web-1",
        Some(t + 7200),
        "You've hit your monthly spend limit",
        t,
    );
    assert_eq!((s.until, s.at, s.job.as_str()), (t + 7200, t, "web-1"));
    assert_eq!(out(&cfg), Some(s.clone()));
    let open = told(&d);
    assert_eq!(open.len(), 1);
    assert!(
        open[0].starts_with("open proj-usage Claude is out of usage until "),
        "{open:?}"
    );
    // Every turn running is refused too: told once, and out till the latest
    // reset any of them gives, from the first refusal.
    let s2 = record(
        &cfg,
        "web-2",
        Some(t + 3600),
        "You've hit your monthly spend limit",
        t + 5,
    );
    assert_eq!((s2.until, s2.at), (t + 7200, t));
    let s3 = record(
        &cfg,
        "web-3",
        Some(t + 9000),
        "You've hit your monthly spend limit",
        t + 6,
    );
    assert_eq!(s3.until, t + 9000);
    assert_eq!(told(&d).len(), 1);
    // A run that isn't refused: back, and the notice closed; once.
    back(&cfg);
    back(&cfg);
    assert_eq!(out(&cfg), None);
    assert_eq!(
        told(&d)[1..],
        ["close proj-usage Claude has usage again, and proj's jobs are running.".to_owned()]
    );
    // A reset unsaid, or already past: an hour on.
    assert_eq!(
        record(&cfg, "web-4", None, "Claude AI usage limit reached", t).until,
        t + UNSAID
    );
    back(&cfg);
    assert_eq!(record(&cfg, "web-4", Some(t - 5), "x", t).until, t + UNSAID);
    // Past its reset it's no longer out, though no run has said so yet.
    fs::write(
        cfg.jobs.join(FILE),
        json!({"until": t - 1, "said": "x", "job": "web-5", "at": t - 9}).to_string(),
    )
    .unwrap();
    assert_eq!(out(&cfg), None);
}
