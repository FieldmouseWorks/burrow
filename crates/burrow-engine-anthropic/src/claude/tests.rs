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
    let (until, said) = refused(&v(RESULT), &stream, NOW).unwrap();
    // Nov 4, 12pm PST.
    assert_eq!(until, Some(1_793_822_400));
    // Its words say so too, for a stream that has no timestamp.
    assert_eq!(refused(&v(RESULT), "", NOW).unwrap().0, Some(1_793_822_400));
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
            Some((
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
        let (until, said) = refused(&v(RESULT), &event.to_string(), NOW).unwrap();
        assert_eq!(until, Some(NOW + 100));
        assert_eq!(said, format!("Claude reached its {name}"));
        assert!(!said.contains("raise"));
        assert!(!said.contains("https://"));
    }
}

/// The run every Claude job on remi ended on from 2026-11-16 11:54 UTC, its
/// stream cut down to what's read: the login had expired.
const LOGIN_ASSISTANT: &str = r#"{"type":"assistant","message":{"model":"<synthetic>","role":"assistant","content":[{"type":"text","text":"Failed to authenticate: OAuth session expired and could not be refreshed"}]},"session_id":"c812a143","error":"authentication_failed","is_api_error_message":true}"#;
const LOGIN_RESULT: &str = r#"{"type":"result","subtype":"success","is_error":true,"num_turns":1,"terminal_reason":"api_error","api_error_status":null,"result":"Failed to authenticate: OAuth session expired and could not be refreshed","session_id":"c812a143","total_cost_usd":0}"#;

#[test]
fn a_run_its_login_expired_under_is_refused_with_no_reset() {
    let logged_out = Some((None, super::LOGGED_OUT.to_owned()));
    let stream = format!("{ALLOWED}\n{LOGIN_ASSISTANT}\n{LOGIN_RESULT}\n");
    assert_eq!(refused(&v(LOGIN_RESULT), &stream, NOW), logged_out);
    // By its words alone, for a stream that doesn't keep the answer's record.
    assert_eq!(refused(&v(LOGIN_RESULT), "", NOW), logged_out);
    // By that record's mark alone, whatever Claude Code's words for it.
    let words = "Invalid API key · Please run /login";
    let marked = json!({"type": "assistant", "error": "authentication_failed",
        "message": {"content": [{"type": "text", "text": words}]}});
    let result = json!({"type": "result", "is_error": true, "result": words});
    assert_eq!(refused(&result, &marked.to_string(), NOW), logged_out);
    // Never a limit's hold: its words reach the owner as a login's.
    assert_eq!(
        super::normalize_refusal(super::LOGGED_OUT),
        super::LOGGED_OUT
    );
    // An answer that talks of one isn't one, nor is an error that only
    // follows one the run got past, nor another failure's record.
    let answer = json!({"type": "result", "is_error": false,
        "result": "Failed to authenticate: so I fixed the token's scope."});
    assert_eq!(refused(&answer, LOGIN_ASSISTANT, NOW), None);
    let later = json!({"type": "result", "is_error": true, "result": "API Error: 529 overloaded"});
    assert_eq!(refused(&later, LOGIN_ASSISTANT, NOW), None);
    let other = json!({"type": "assistant", "error": "server_error",
        "message": {"content": [{"type": "text", "text": "API Error: 529 overloaded"}]}});
    assert_eq!(refused(&later, &other.to_string(), NOW), None);
}
