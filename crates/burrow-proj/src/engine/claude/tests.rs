use super::refused;
use serde_json::{Value, json};

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
    let super::Refusal { until, said, login } = refused(&v(RESULT), &stream, NOW).unwrap();
    assert!(!login);
    // Nov 4, 12pm PST.
    assert_eq!(until, Some(1_793_822_400));
    // Its words say so too, for a stream that has no timestamp.
    assert_eq!(
        refused(&v(RESULT), "", NOW).unwrap().until,
        Some(1_793_822_400)
    );
    assert!(said == "Claude reached its weekly allowance", "{said}");
    // Claude Code's other limits, by their words alone, the older one with
    // its reset after a bar, one with its zone read by those words.
    for (said, until) in [
        (
            "Claude AI usage limit reached|1793822400",
            Some(1_793_822_400),
        ),
        ("5-hour limit reached ∙ resets 3pm", None),
        ("weekly limit reached ∙ resets Mon 9am", None),
        ("You've reached your usage limit.", None),
        (
            "You've hit your limit · resets 3pm (America/Los_Angeles)",
            Some(1_793_660_400),
        ),
    ] {
        let r = json!({"type": "result", "is_error": true, "result": said});
        assert_eq!(
            refused(&r, "", NOW),
            Some(super::Refusal::usage(
                until,
                format!("Claude reached its {}", super::limit_name(said))
            )),
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

#[test]
fn structured_limit_names_override_billing_words() {
    for (kind, overage, name) in [
        ("seven_day", "rejected", "weekly allowance"),
        ("five_hour", "rejected", "five-hour allowance"),
        ("overage", "rejected", "extra-usage cap"),
    ] {
        let event = json!({"rate_limit_info": {"status": "rejected", "resetsAt": NOW + 100,
            "rateLimitType": kind, "overageStatus": overage, "isUsingOverage": false}});
        let super::Refusal { until, said, .. } =
            refused(&v(RESULT), &event.to_string(), NOW).unwrap();
        assert_eq!(until, Some(NOW + 100));
        assert_eq!(said, format!("Claude reached its {name}"));
        assert!(!said.contains("raise"));
        assert!(!said.contains("https://"));
    }
}

/// A run refused for its login, as Claude Code wrote it on 2026-11-12 (cut
/// down to what's read): the API's refusal as a message of its own, with the
/// stream's `error`, then the run's result.
pub(crate) const LOGIN: &str = concat!(
    r#"{"type":"assistant","message":{"model":"<synthetic>","role":"assistant","content":[{"type":"text","text":"Failed to authenticate: OAuth session expired and could not be refreshed"}]},"session_id":"d1bb1794","error":"authentication_failed","is_api_error_message":true}"#,
    "\n",
    r#"{"type":"result","subtype":"success","is_error":true,"num_turns":1,"api_error_status":null,"result":"Failed to authenticate: OAuth session expired and could not be refreshed","session_id":"d1bb1794"}"#,
    "\n"
);

// A login refused is told by the stream's own `error`, never by its words.
#[test]
fn a_refused_login_is_told_by_its_error() {
    let result = v(LOGIN.lines().last().unwrap());
    assert_eq!(
        refused(&result, LOGIN, NOW),
        Some(super::Refusal {
            until: None,
            said: "Failed to authenticate: OAuth session expired and could not be refreshed".into(),
            login: true,
        })
    );
    // The same words with no such error: an answer, not a refusal.
    assert_eq!(refused(&result, "", NOW), None);
    // The error on a run that went on to answer: that run got through.
    let answered = json!({"type": "result", "is_error": false, "result": "done"});
    assert_eq!(refused(&answered, LOGIN, NOW), None);
    // A usage limit refused with it is a usage limit: it says when it resets.
    let both = format!("{LOGIN}{ASSISTANT}\n");
    let r = refused(&v(RESULT), &both, NOW).unwrap();
    assert_eq!((r.until, r.login), (Some(1_793_822_400), false));
}
