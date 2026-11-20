use std::fs;

use super::watch::gone_quiet;
use super::{github_app, stopped};

// A turn acts as the user unless its job names an App.
#[test]
fn a_turn_acts_as_its_jobs_app_or_the_user() {
    assert_eq!(github_app(None), None);
    assert_eq!(github_app(Some(String::new())), None);
    assert_eq!(github_app(Some("other".into())), Some("other".into()));
}

// A sandbox's VM counts as stopped, everything a refused run left in it
// with it, only once `agent stop` says so: one that fails, or can't run,
// and the turn doesn't go on.
#[test]
fn a_sandbox_stopped_only_when_agent_says_so() {
    let d = tempfile::tempdir().unwrap();
    let agent = |code: i32| {
        let f = d.path().join(format!("agent-{code}"));
        let script = format!(
            "#!/bin/sh\necho \"$*\" >>{}/asked\nexit {code}\n",
            d.path().display()
        );
        burrow_test::executable(&f, script);
        f.to_str().unwrap().to_owned()
    };
    assert!(stopped(&agent(0), "web-ab12"));
    assert!(!stopped(&agent(1), "web-ab12"));
    let missing = d.path().join("no-agent");
    assert!(!stopped(missing.to_str().unwrap(), "web-ab12"));
    let asked = fs::read_to_string(d.path().join("asked")).unwrap();
    assert_eq!(asked, "stop web-ab12\nstop web-ab12\n");
}

const SECOND: i64 = 1_000_000_000;

#[test]
fn quiet_is_all_of_idle_wherever_in_a_second_the_stream_last_spoke() {
    let spoke = 100 * SECOND + 900_000_000;
    // The clock's second turned over fifteen times, in 14.1s of quiet.
    assert!(!gone_quiet(115 * SECOND, spoke, 15 * SECOND));
    assert!(!gone_quiet(spoke + 15 * SECOND - 1, spoke, 15 * SECOND));
    assert!(gone_quiet(spoke + 15 * SECOND, spoke, 15 * SECOND));
}

#[test]
fn no_idle_never_goes_quiet() {
    assert!(!gone_quiet(1000 * SECOND, 0, 0));
}

/// A turn the credit refused ends with the refusal as its result and
/// outcome, in words that name the credit, not the plan's usage.
#[test]
fn an_api_refusal_ends_the_turn_in_its_own_words() {
    use super::{Config, Job, api_refused, engine};
    let dir = tempfile::tempdir().unwrap();
    let mut cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
    cfg.jobs = dir.path().join("jobs");
    let job = Job::new(&cfg, "job");
    fs::create_dir_all(&job.dir).unwrap();
    let told = std::cell::RefCell::new(String::new());
    let refused = engine::api::Refused::new("its credit has no room: today's allowance spent");
    api_refused(&job, "1", &refused, &|t| told.borrow_mut().push_str(t));
    let said = "claude-api refused: its credit has no room: today's allowance spent";
    assert_eq!(*told.borrow(), said);
    assert_eq!(
        fs::read_to_string(job.turn("1", "result")).unwrap(),
        format!("{said}\n")
    );
    assert!(!cfg.jobs.join(crate::usage::FILE).exists());
}

/// A retry the credit won't admit keeps the answer an earlier run of the
/// turn gave; with none, or only an error, the refusal is the turn's end.
#[test]
fn a_refused_retry_keeps_the_answer_the_turn_has() {
    use super::answered;
    let dir = tempfile::tempdir().unwrap();
    let stream = dir.path().join("1.jsonl");
    fs::write(&stream, "").unwrap();
    assert_eq!(answered(&stream), None);
    let error = r#"{"type":"result","is_error":true,"result":"broke"}"#;
    fs::write(
        &stream,
        format!("{{\"type\":\"burrow.run\",\"at\":1}}\n{error}\n"),
    )
    .unwrap();
    assert_eq!(answered(&stream), None);
    let good = r#"{"type":"result","is_error":false,"result":"done, no next step said"}"#;
    fs::write(
        &stream,
        format!("{{\"type\":\"burrow.run\",\"at\":1}}\n{good}\n"),
    )
    .unwrap();
    assert_eq!(
        answered(&stream).unwrap()["result"],
        "done, no next step said"
    );
}
