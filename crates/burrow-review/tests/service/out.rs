//! Out of service: the plan's usage spent (asked again within the hour,
//! since credits carry it before the plan's reset; that soon, if sooner),
//! logged out. Any other failure is just a failure, and nothing an agent
//! says carries its login's keys.

use std::fs;
use std::time::Instant;

use burrow_review::now;
use serde_json::{Value, json};

use crate::rig::{HEAD, Rig};

/// One look with the stand-in doing `todo`: what came back.
fn one(rig: &Rig, todo: Value) -> Value {
    let snap = rig.snapshot(5, HEAD);
    rig.agent(todo);
    let entry = rig.queue(&snap, 5, HEAD, json!({}));
    rig.work();
    assert!(rig.names("queue").is_empty());
    rig.back(&entry)
}

fn weekday_time(at: f64) -> String {
    jiff::Timestamp::from_second(at as i64)
        .unwrap()
        .strftime("%a %H:%MZ")
        .to_string()
}

#[test]
fn a_spent_plan_is_out_until_it_says_or_an_hour() {
    let rig = Rig::new();
    let t = now();
    let back = one(
        &rig,
        json!({"rc": 1, "err": "ERROR: You've hit your usage limit. Upgrade to Pro or try again in 2 days 3 hours."}),
    );
    assert_eq!(back["result"], "out", "{back}");
    assert!(
        (back["until"].as_f64().unwrap() - t - 3600.0).abs() < 60.0,
        "{back}"
    );
    let why = back["why"].as_str().unwrap();
    assert!(why.contains("usage"), "{why}");
    assert!(
        [0.0, 60.0]
            .iter()
            .any(|d| why.contains(&weekday_time(t + 2.0 * 86400.0 + 3.0 * 3600.0 + d))),
        "{why}"
    );
    let back = one(
        &rig,
        json!({"rc": 1, "err": "ERROR: You've hit your usage limit. Upgrade to Pro or try again in 20 minutes."}),
    );
    assert_eq!(back["result"], "out", "{back}");
    assert!(
        (back["until"].as_f64().unwrap() - t - 1200.0).abs() < 60.0,
        "{back}"
    );
    // Said in an error event, it counts too.
    let back = one(
        &rig,
        json!({"rc": 1, "err": "", "events": [{"type": "turn.failed", "error": {"message": "You've hit your usage limit."}}]}),
    );
    assert_eq!(back["result"], "out", "{back}");
}

/// An answer that only mentions a usage limit is a review.
#[test]
fn an_answer_that_mentions_a_limit_is_a_review() {
    let rig = Rig::new();
    let finding = json!({"path": "a", "line": 1, "severity": "P2", "title": "t", "body": "no usage limit on retries"});
    let back = one(
        &rig,
        json!({"answer": {"findings": [finding], "summary": "s"}}),
    );
    assert_eq!(back["result"], "review", "{back}");
    assert_eq!(
        back["answer"]["findings"][0]["body"],
        "no usage limit on retries"
    );
}

#[test]
fn logged_out_is_out_for_the_login_to_be_redone() {
    let rig = Rig::new();
    let t = now();
    let back = one(
        &rig,
        json!({"rc": 1, "err": "Error: Not logged in. Run codex login."}),
    );
    assert_eq!(back["result"], "out", "{back}");
    assert!(
        back["why"]
            .as_str()
            .unwrap()
            .contains("autoland-codex-login")
            && back["until"].as_f64().unwrap() > t,
        "{back}"
    );
    let back = one(
        &rig,
        json!({"rc": 1, "err": "", "events": [{"type": "error", "message": "unexpected status 401 Unauthorized"}]}),
    );
    assert_eq!(back["result"], "out", "{back}");
    assert!(
        back["why"]
            .as_str()
            .unwrap()
            .contains("autoland-codex-login"),
        "{back}"
    );
}

/// Any other failure is a failure, said in its own words: not in what it
/// read on the way (the PR's own files, in its events), nor in a 401 inside
/// a longer number.
#[test]
fn any_other_failure_is_a_failure() {
    let rig = Rig::new();
    let back = one(&rig, json!({"rc": 2, "err": "panic: something else"}));
    assert_eq!(back["result"], "error", "{back}");
    assert!(
        back["error"].as_str().unwrap().contains("something else"),
        "{back}"
    );
    let read = json!({"type": "item.completed", "item": {"type": "command_execution", "aggregated_output": "usage limit; not logged in; 401"}});
    let back = one(
        &rig,
        json!({"rc": 1, "err": "no session session-1793040155", "events": [read]}),
    );
    assert_eq!(back["result"], "error", "{back}");
    assert!(
        back["error"].as_str().unwrap().contains("no session"),
        "{back}"
    );
    // An answer with no JSON in it is no answer.
    let back = one(&rig, json!({"say": "I looked, and it's fine."}));
    assert_eq!(back["result"], "error", "{back}");
    assert!(
        back["error"]
            .as_str()
            .unwrap()
            .contains("no JSON in its answer"),
        "{back}"
    );
}

/// One still at it past its time is stopped, and it's a failed try.
#[test]
fn a_look_past_its_time_is_stopped() {
    let mut rig = Rig::new();
    rig.cfg["timeoutMinutes"] = json!(0.02); // a second and a bit
    let t = Instant::now();
    let back = one(&rig, json!({"sleep": 30}));
    assert_eq!(back["result"], "error", "{back}");
    assert!(
        back["error"]
            .as_str()
            .unwrap()
            .contains("still at it after 0.02 minutes"),
        "{back}"
    );
    assert!(t.elapsed().as_secs() < 20, "{:?}", t.elapsed());
}

/// Its login's keys never come out of it, whole or cut in half: not in the
/// journal, not in what's kept for the steward's next pass.
#[test]
fn its_keys_stay_out_of_what_it_says() {
    let rig = Rig::new();
    let token = "codexREFRESHtokenQWERTYzxcvb";
    fs::write(
        rig.home().join(".codex/auth.json"),
        json!({"tokens": {"refresh_token": token}}).to_string(),
    )
    .unwrap();
    for (todo, redacted) in [
        (
            json!({"rc": 2, "err": format!("panic: {token} {}", "x".repeat(280))}),
            true,
        ),
        (json!({"say": format!("I can't, {token}")}), true),
        (
            json!({"rc": 1, "err": format!("You've hit your usage limit, {token}.")}),
            false,
        ),
    ] {
        rig.journal.lock().unwrap().clear();
        let back = one(&rig, todo.clone());
        let said = format!("{}\n{back}", rig.said());
        assert!(
            back["result"] != "review" && said.contains("[redacted]") == redacted,
            "{todo}: {said}"
        );
        assert!(
            !(0..token.len() - 7).any(|i| said.contains(&token[i..i + 8])),
            "{todo}: {said}"
        );
    }
    let finding = json!({"path": "a", "line": 1, "severity": "P1", "title": token, "body": format!("it said {token}")});
    let back = one(
        &rig,
        json!({"answer": {"findings": [finding], "summary": token}}),
    );
    assert_eq!(back["result"], "review", "{back}");
    assert!(
        !back.to_string().contains(token) && back.to_string().contains("[redacted]"),
        "{back}"
    );
}
