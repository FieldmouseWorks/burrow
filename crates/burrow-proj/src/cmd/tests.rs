use std::fs;

use std::env;

use std::process::Command;

use super::{baselines, left_be};
use crate::config::Config;
use crate::job::Job;

// proj withdraw stops only a turn that says it may be, and has no slot: one
// that has started or ended it leaves be, saying so, and one from an older
// proj, or one systemd can't say about, it fails on, to be asked about again.
#[test]
fn what_withdraw_leaves_be() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut v = crate::config::tests::example();
    v["jobs"] = tmp.path().to_str().unwrap().into();
    let cfg: Config = serde_json::from_value(v).unwrap();
    fs::create_dir(tmp.path().join("j")).unwrap();
    fs::write(tmp.path().join("j/turns"), "1\n").unwrap();
    let job = Job::new(&cfg, "j");
    let said = |active| left_be(&job, "1", active).map_err(|f| f.code);

    for active in ["inactive", "failed"] {
        assert_eq!(said(active), Ok(Some("over")), "{active}");
    }
    for active in ["", "deactivating", "maintenance"] {
        assert_eq!(said(active), Err(1), "{active:?}");
    }
    assert_eq!(
        said("active"),
        Err(1),
        "unsaid: it isn't known to take its slot locked"
    );
    fs::write(job.turn("1", "withdrawable"), "").unwrap();
    for active in ["active", "activating", "reloading"] {
        assert_eq!(said(active), Ok(None), "{active}");
    }
    fs::write(job.turn("1", "slot"), "2\n").unwrap();
    assert_eq!(said("active"), Ok(Some("running")));
}

// A baseline GitHub can't give fails the whole await, and writes nothing.
#[test]
fn an_await_needs_every_baseline() {
    let mut v = crate::config::tests::example();
    v["tools"]["gh"] = "/nowhere/gh".into();
    let cfg: Config = serde_json::from_value(v).unwrap();
    let found = [("o/r".to_owned(), "5".to_owned())];
    let fail = baselines(&cfg, &found, &["o/r#5".to_owned()], None).unwrap_err();
    assert_eq!(fail.code, 1);
    assert!(
        fail.say
            .unwrap()
            .contains("couldn't read o/r#5 from GitHub")
    );
}

// An await on a PR that's merged, or one or an issue that's closed, fails
// the whole await and writes nothing: its baseline says what it will say
// until awaitHours runs out, so the job would sit until then for nothing.
// The stand-in gh is written and run where nothing else forks, as
// slot::tests' stand-in git is.
#[test]
fn an_await_on_what_is_over_is_refused() {
    let status = Command::new(env::current_exe().unwrap())
        .args([
            "--exact",
            "cmd::tests::awaits_against_a_stand_in_gh",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("BURROW_PROJ_TEST_AWAIT", "1")
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&status.stdout);
    assert!(status.status.success(), "{out}");
    assert!(out.contains("1 passed"), "{out}");
}

#[test]
fn awaits_against_a_stand_in_gh() {
    if env::var_os("BURROW_PROJ_TEST_AWAIT").is_none() {
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let gh = tmp.path().join("gh");
    // #5 a merged PR, #6 a PR closed without merging, #7 a closed
    // issue, #8 an open PR.
    burrow_test::executable(
        &gh,
        r#"#!/bin/sh
for a; do ep=${a%%\?*}; done
printf 'HTTP/2.0 200 OK\n\n'
case $ep in
  */comments | */reviews) echo '[]' ;;
  */status) echo '{"total_count": 0, "state": "pending"}' ;;
  */check-runs) echo '{"check_runs": []}' ;;
  repos/o/r/issues/7) echo '{"state": "closed"}' ;;
  repos/o/r/issues/8) echo '{"pull_request": {}, "state": "open"}' ;;
  repos/o/r/issues/*) echo '{"pull_request": {}, "state": "closed"}' ;;
  repos/o/r/pulls/5) echo '{"state": "closed", "merged": true, "head": {"sha": "abc"}, "review_comments": 0}' ;;
  repos/o/r/pulls/6) echo '{"state": "closed", "merged": false, "head": {"sha": "abc"}, "review_comments": 0}' ;;
  repos/o/r/pulls/8) echo '{"state": "open", "merged": false, "head": {"sha": "abc"}, "review_comments": 0}' ;;
  *) exit 1 ;;
esac
"#,
    );
    let mut v = crate::config::tests::example();
    v["tools"]["gh"] = gh.to_str().unwrap().into();
    let cfg: Config = serde_json::from_value(v).unwrap();
    let await_on = |ns: &[&str]| {
        let found: Vec<(String, String)> = ns
            .iter()
            .map(|n| ("o/r".to_owned(), (*n).to_owned()))
            .collect();
        let targets: Vec<String> = ns.iter().map(|n| format!("o/r#{n}")).collect();
        baselines(&cfg, &found, &targets, None).map_err(|f| (f.code, f.say.unwrap_or_default()))
    };

    let open = await_on(&["8"]).unwrap();
    assert!(open.starts_with("o/r#8\t"), "{open}");
    assert!(open.contains(r#""merged":false"#), "{open}");
    for (n, how) in [("5", "merged"), ("6", "closed"), ("7", "closed")] {
        let (code, say) = await_on(&[n]).unwrap_err();
        assert_eq!(code, 1, "o/r#{n}");
        assert!(
            say.contains(&format!("o/r#{n} is {how} already, so nothing is awaited")),
            "o/r#{n}: {say}"
        );
    }
    // One that's over fails the lot, open ones with it.
    let (_, say) = await_on(&["8", "5"]).unwrap_err();
    assert!(say.contains("o/r#5 is merged already"), "{say}");
}
