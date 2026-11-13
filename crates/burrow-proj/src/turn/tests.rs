use std::fs;

use super::{github_app, gone_quiet, stopped};
use crate::config::Config;
use crate::engine::Engine;

// A Codex turn reaches GitHub as an App, its job's own or the machine's,
// never as the user, whose logins its sandbox hides; with neither it
// doesn't start. Claude's acts as the user unless its job names an App.
#[test]
fn a_codex_turn_acts_as_an_app_or_not_at_all() {
    let mut cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
    let app = |own: Option<&str>, engine| github_app(&cfg, own.map(str::to_owned), engine);
    assert_eq!(app(None, Engine::Codex), Ok(Some("workers".into())));
    assert_eq!(app(Some(""), Engine::Codex), Ok(Some("workers".into())));
    assert_eq!(app(Some("other"), Engine::Codex), Ok(Some("other".into())));
    assert_eq!(app(None, Engine::Claude), Ok(None));
    assert_eq!(app(Some("other"), Engine::Claude), Ok(Some("other".into())));
    for none in [None, Some(String::new())] {
        cfg.machinery = none;
        let app = |own: Option<&str>, engine| github_app(&cfg, own.map(str::to_owned), engine);
        let refused = app(None, Engine::Codex).unwrap_err();
        assert!(refused.contains("only as a GitHub App"), "{refused}");
        assert_eq!(app(Some("other"), Engine::Codex), Ok(Some("other".into())));
        assert_eq!(app(None, Engine::Claude), Ok(None));
    }
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

// A handed-off turn's successor takes over what the old turn registered: the
// quota registration and the question recommendation name it, not the old
// turn, and so aren't dropped as another turn's leftovers.
#[test]
fn a_handoff_retargets_the_waiting_turns_records() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut v = crate::config::tests::example();
    v["jobs"] = tmp.path().to_str().unwrap().into();
    let cfg: Config = serde_json::from_value(v).unwrap();
    fs::create_dir(tmp.path().join("j")).unwrap();
    let job = crate::job::Job::new(&cfg, "j");
    let pending = crate::github_quota::Pending {
        targets: vec!["o/r#5".into()],
        retry_at: 7,
        turn: "1".into(),
        baselines: "b".into(),
    };
    fs::write(
        job.path(crate::github_quota::PENDING),
        serde_json::to_string(&pending).unwrap(),
    )
    .unwrap();
    fs::write(
        job.path("ask.next"),
        r#"{"default":"go","one_way":true,"turn":"1"}"#,
    )
    .unwrap();
    super::retarget(&job, "1", "2");
    let p: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(job.path(crate::github_quota::PENDING)).unwrap())
            .unwrap();
    assert_eq!(
        (
            p["turn"].as_str(),
            p["baselines"].as_str(),
            p["retry_at"].as_u64()
        ),
        (Some("2"), Some("b"), Some(7))
    );
    let a: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(job.path("ask.next")).unwrap()).unwrap();
    assert_eq!(
        (a["turn"].as_str(), a["one_way"].as_bool()),
        (Some("2"), Some(true))
    );
    // Another turn's records are left be.
    super::retarget(&job, "9", "3");
    assert!(
        fs::read_to_string(job.path("ask.next"))
            .unwrap()
            .contains(r#""turn":"2""#)
    );
}
