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
