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

#[test]
fn usage_is_one_json_value_with_zero_one_or_multiple_github_readings() {
    if let Ok(kind) = env::var("PROJ_TEST_USAGE_JSON") {
        let d = tempfile::tempdir().unwrap();
        let mut cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
        cfg.jobs = d.path().join("jobs");
        cfg.github_meters = Some(d.path().join("host-meters"));
        fs::create_dir_all(cfg.github_meters.as_ref().unwrap()).unwrap();
        cfg.notify = None;
        fs::create_dir_all(&cfg.jobs).unwrap();
        let count: usize = kind.split('-').next().unwrap().parse().unwrap();
        if !kind.ends_with("none") {
            let now = crate::now();
            let until = if kind.ends_with("expired") {
                now - 1
            } else {
                now + 3600
            };
            fs::write(
                cfg.jobs.join(".usage.json"),
                serde_json::json!({"until":until,"said":"held","job":"fixture","at":now-10})
                    .to_string(),
            )
            .unwrap();
        }
        for n in 0..count {
            let id = format!("j{n}");
            let job = Job::new(&cfg, &id);
            fs::create_dir_all(&job.dir).unwrap();
            burrow_github::quota::Meter::new("workers", &format!("owner{n}"), crate::now() as u64)
                .record(cfg.github_meters.as_ref().unwrap())
                .unwrap();
        }
        super::dispatch(&cfg, &["usage".into()]).unwrap();
        return;
    }
    for kind in ["0-held", "1-held", "2-held", "2-none", "2-expired"] {
        let out = Command::new(env::current_exe().unwrap())
            .args([
                "--exact",
                "cmd::tests::usage_is_one_json_value_with_zero_one_or_multiple_github_readings",
                "--nocapture",
            ])
            .env("PROJ_TEST_USAGE_JSON", kind)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        // The libtest harness surrounds the command's stdout; the JSON
        // line itself must be a single value, not an appended JSON stream.
        let stdout = String::from_utf8(out.stdout).unwrap();
        let lines: Vec<_> = stdout
            .lines()
            .filter_map(|line| line.find('{').map(|start| &line[start..]))
            .collect();
        assert_eq!(lines.len(), 1, "{stdout}");
        let value: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        let active = kind.ends_with("held");
        assert_eq!(
            value["until"].as_i64().is_some_and(|t| t > crate::now()),
            active,
            "{stdout}"
        );
        let count = kind[..1].parse::<usize>().unwrap();
        assert_eq!(value["github"].as_array().map_or(0, Vec::len), count);
    }
}

// A handoff's stop of the turn it replaces is no end of the job while the
// handoff is in progress: nothing reads the turn as stopped for good, the job
// still holds, and the scratch stays. Once its successor is accepted the turn
// keeps a marker nothing reads, and a stop (the job's end) retires the record.
#[test]
fn a_handoff_in_progress_is_not_a_stop() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut v = crate::config::tests::example();
    v["jobs"] = tmp.path().to_str().unwrap().into();
    let cfg: Config = serde_json::from_value(v).unwrap();
    fs::create_dir(tmp.path().join("j")).unwrap();
    fs::write(tmp.path().join("j/turns"), "1\n").unwrap();
    let job = Job::new(&cfg, "j");
    fs::write(job.turn("1", "stopped"), "").unwrap();
    assert!(job.stopped_for_good("1"), "a stop is final");
    fs::write(job.path(crate::scratch::HANDOFF), "1\n").unwrap();
    assert!(!job.stopped_for_good("1"));
    assert!(job.holds().unwrap().contains(&"handoff".to_owned()));
    assert!(!crate::scratch::over(&job));

    // Interrupted before its successor: nothing to settle yet.
    super::handoff::settle_handoff(&job).unwrap();
    assert!(job.turn("1", "stopped").exists());
    assert!(job.path(crate::scratch::HANDOFF).exists());

    // Its successor accepted: the turn's stop is history, not a stop.
    fs::write(job.path("turns"), "2\n").unwrap();
    super::handoff::settle_handoff(&job).unwrap();
    assert!(!job.turn("1", "stopped").exists());
    assert!(job.turn("1", "handed-off").exists());
    assert!(!job.stopped_for_good("1"));
    assert!(!job.path(crate::scratch::HANDOFF).exists());
}

// A withdrawal of a job whose handoff stopped its turn but never started the
// next finishes the job: the question and waits go, the scratch is cleared and
// the handoff's record goes with them, last. It isn't taken for a turn that
// merely isn't waiting.
#[test]
fn withdrawing_a_stranded_handoff_cancels_everything() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut v = crate::config::tests::example();
    v["jobs"] = tmp.path().join("jobs").to_str().unwrap().into();
    v["scratch"] = tmp.path().join("scratch").to_str().unwrap().into();
    let mut cfg: Config = serde_json::from_value(v).unwrap();
    cfg.tools.systemctl = tmp.path().join("systemctl");
    burrow_test::executable(
        &cfg.tools.systemctl,
        "#!/bin/sh\ncase \"$*\" in *is-active*) exit 3 ;; *LoadState*) echo not-found ;; *ActiveState*) echo inactive ;; esac\n",
    );
    fs::create_dir_all(tmp.path().join("jobs/j")).unwrap();
    fs::create_dir_all(tmp.path().join("scratch/jobs/j")).unwrap();
    fs::write(tmp.path().join("scratch/jobs/j/f"), "x").unwrap();
    let job = Job::new(&cfg, "j");
    fs::write(job.path("turns"), "1\n").unwrap();
    fs::write(job.turn("1", "stopped"), "").unwrap();
    fs::write(job.path(crate::scratch::HANDOFF), "1\n").unwrap();
    fs::write(job.path("await"), "o/r#1\t0\t{}\n").unwrap();
    super::halt(&cfg, &job, true).unwrap();
    assert!(!job.path("await").exists(), "its waits are cancelled");
    assert!(!job.path(crate::scratch::HANDOFF).exists());
    assert!(
        !tmp.path().join("scratch/jobs/j").exists(),
        "its scratch goes"
    );
}

// A withdrawal that can't act (the turn isn't waiting) leaves a handoff alone.
#[test]
fn a_failed_stop_keeps_the_handoff() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut v = crate::config::tests::example();
    v["jobs"] = tmp.path().to_str().unwrap().into();
    let mut cfg: Config = serde_json::from_value(v).unwrap();
    cfg.tools.systemctl = tmp.path().join("systemctl");
    burrow_test::executable(
        &cfg.tools.systemctl,
        "#!/bin/sh\ncase \"$*\" in *is-active*) exit 0 ;; *LoadState*) echo loaded ;; *ActiveState*) echo active ;; *stop*) exit 1 ;; esac\n",
    );
    fs::create_dir(tmp.path().join("j")).unwrap();
    let job = Job::new(&cfg, "j");
    fs::write(job.path("turns"), "1\n").unwrap();
    fs::write(job.path(crate::scratch::HANDOFF), "1\n").unwrap();
    assert!(super::halt(&cfg, &job, false).is_err());
    assert!(job.path(crate::scratch::HANDOFF).exists());
}

// A withdrawal keeps its no-slot safeguard once a handoff's successor was
// accepted, or can't be told: only a successor known never accepted unwinds.
#[test]
fn withdrawing_leaves_an_accepted_or_unknown_handoff_successor_be() {
    for (stub, slot) in [("echo not-found", true), ("exit 1", false)] {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut v = crate::config::tests::example();
        v["jobs"] = tmp.path().to_str().unwrap().into();
        let mut cfg: Config = serde_json::from_value(v).unwrap();
        cfg.tools.systemctl = tmp.path().join("systemctl");
        burrow_test::executable(
            &cfg.tools.systemctl,
            format!(
                "#!/bin/sh\ncase \"$*\" in *ActiveState*) echo active ;; *LoadState*) {stub} ;; esac\n"
            ),
        );
        fs::create_dir(tmp.path().join("j")).unwrap();
        let job = Job::new(&cfg, "j");
        fs::write(job.path("turns"), "2\n").unwrap();
        fs::write(job.turn("1", "stopped"), "").unwrap();
        fs::write(job.path(crate::scratch::HANDOFF), "1\n").unwrap();
        if slot {
            fs::write(job.turn("2", "slot"), "").unwrap();
        }
        super::halt(&cfg, &job, true).unwrap_or(());
        assert!(
            job.path(crate::scratch::HANDOFF).exists(),
            "the handoff and the running successor are left be"
        );
        assert!(!job.turn("2", "stopped").exists());
    }
}
