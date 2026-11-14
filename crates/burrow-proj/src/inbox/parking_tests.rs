//! Parking wins over both inbox passes and resumes that beat the label event.
use super::sorting_tests::{Box, issue};
use crate::{ask, job::Job, read};
use serde_json::json;
use std::fs;

pub(super) fn on_github(b: &Box, issue: &serde_json::Value) {
    let mut issue = issue.clone();
    issue["repository_url"] = json!("https://api.github.com/repos/sam/box");
    super::sorting_tests::on_github(b, &issue);
}

pub(super) fn fixture() -> Box {
    let mut b = Box::new();
    b.cfg.kick.clear();
    b.cfg.notify = None;
    b.cfg.wake = b.path("watch-wake.json");
    b.cfg.projects = b.path("projects");
    fs::create_dir_all(b.cfg.projects.join("box")).unwrap();
    burrow_test::executable(
        &b.cfg.tools.systemctl,
        "#!/bin/sh\ncase \"$*\" in\n*is-active*) exit 3;;\n*LoadState*) echo not-found;;\n*ActiveState*) echo inactive;;\nesac\n",
    );
    b.cfg.tools.systemd_run = b.path("systemd-run");
    burrow_test::executable(
        &b.cfg.tools.systemd_run,
        format!("#!/bin/sh\ntouch '{}'\n", b.path("launched").display()),
    );
    b.job_ending("j1", "done", Some("await"), "Waiting on the PR.");
    b.record("j1");
    let job = Job::new(&b.cfg, "j1");
    for (name, text) in [
        ("inbox-issue", "true"),
        ("link", "https://github.com/sam/box/issues/7"),
        ("session", "conversation"),
        ("await", "sam/box#8\t1\t{}\n"),
    ] {
        fs::write(job.path(name), text).unwrap();
    }
    b
}

#[test]
fn parking_an_awaiting_issue_stops_its_job_and_cancels_waits() {
    for labels in [vec!["claude", "later"], vec!["later"], vec![]] {
        let b = fixture();
        let parked = issue(7, "sam", &labels);
        on_github(&b, &parked);
        let listed = if labels.contains(&"claude") {
            vec![parked]
        } else {
            vec![]
        };
        b.pass().jobs("sam/box", "box", &listed).unwrap();
        let job = Job::new(&b.cfg, "j1");
        assert!(job.turn("1", "stopped").exists(), "{labels:?}");
        assert!(!job.path("await").exists());
        assert!(
            !b.cfg
                .inbox
                .as_ref()
                .unwrap()
                .state
                .join("sam_box-7")
                .exists()
        );
        assert_eq!(job.turns().as_deref(), Some("1"));
    }
}

#[test]
fn parking_stops_running_and_policy_parked_issue_jobs_too() {
    for policy_parked in [false, true] {
        let b = fixture();
        let job = Job::new(&b.cfg, "j1");
        if policy_parked {
            crate::policy::mark(&job, "done", false).unwrap();
        } else {
            fs::remove_file(job.turn("1", "outcome")).unwrap();
            fs::remove_file(job.turn("1", "jsonl")).unwrap();
            fs::write(job.turn("1", "slot"), "1").unwrap();
            fs::write(b.path("active"), "").unwrap();
            // Stopping a unit removes its simulated running state.
            burrow_test::executable(
                &b.cfg.tools.systemctl,
                format!(
                    "#!/bin/sh\ncase \"$*\" in\n*is-active*) test -e '{active}';;\n*stop*) rm -f '{active}';;\n*LoadState*) echo not-found;;\n*ActiveState*) if test -e '{active}'; then echo active; else echo inactive; fi;;\nesac\n",
                    active = b.path("active").display(),
                ),
            );
        }
        on_github(&b, &issue(7, "sam", &["later"]));
        b.pass().jobs("sam/box", "box", &[]).unwrap();
        assert!(job.turn("1", "stopped").exists());
        assert!(!job.path("await").exists());
        assert!(!b.path("active").exists());
        assert!(!crate::policy::parked(&job));
    }
}

#[test]
fn an_await_wake_on_a_parked_issue_takes_no_turn() {
    for labels in [vec!["codex", "later"], vec![]] {
        let b = fixture();
        on_github(&b, &issue(7, "sam", &labels));
        on_github(&b, &issue(8, "sam", &["claude"]));
        crate::watch::look(&b.cfg, "sam/box").unwrap();
        let job = Job::new(&b.cfg, "j1");
        assert_eq!(job.turns().as_deref(), Some("1"));
        assert!(!job.path("prompt.2").exists());
        assert!(!b.path("launched").exists());
        assert!(job.turn("1", "stopped").exists());
        assert!(!job.path("await").exists());
        assert_eq!(read(&b.cfg.wake).unwrap().trim(), "{}");
    }
}

#[test]
fn an_ask_deadline_on_a_parked_issue_takes_no_turn() {
    for one_way in [false, true] {
        let b = fixture();
        let job = Job::new(&b.cfg, "j1");
        let q = ask::Question {
            recommendation: ask::Recommendation {
                default: "continue".into(),
                one_way,
                about: None,
            },
            due: crate::now() - 1,
            id: None,
            pauses: Default::default(),
        };
        fs::write(ask::host_path(&job, "question"), json!(q).to_string()).unwrap();
        on_github(&b, &issue(7, "sam", &["claude", "later"]));
        let _held = job.lock().unwrap();
        assert!(ask::expire(&b.cfg, &job).is_err());
        assert_eq!(job.turns().as_deref(), Some("1"));
        assert!(!b.path("launched").exists());
        assert!(job.turn("1", "stopped").exists());
        assert!(ask::load(&job).unwrap().is_none());
    }
}

#[test]
fn relabelling_starts_a_fresh_job_and_never_resumes_the_stopped_one() {
    let b = fixture();
    let parked = issue(7, "sam", &["claude", "later"]);
    on_github(&b, &parked);
    b.pass()
        .jobs("sam/box", "box", std::slice::from_ref(&parked))
        .unwrap();
    b.pass().new_issues("sam/box", &[parked]).unwrap();
    let record = b.cfg.inbox.as_ref().unwrap().state.join("sam_box-7");
    assert!(
        !record.join("job").exists(),
        "later wins over an engine label"
    );

    let ready = issue(7, "sam", &["ready", "codex"]);
    on_github(&b, &ready);
    b.pass().new_issues("sam/box", &[ready]).unwrap();
    let fresh = read(&record.join("job")).unwrap();
    assert_ne!(fresh, "j1");
    let job = Job::new(&b.cfg, &fresh);
    assert_eq!(job.turns().as_deref(), Some("1"));
    assert!(!job.path("session").exists());
    let old = Job::new(&b.cfg, "j1");
    let _held = old.lock().unwrap();
    assert!(crate::turn::start(&b.cfg, "j1", "late wake", false).is_err());
    assert_eq!(old.turns().as_deref(), Some("1"));
}

#[test]
fn removing_one_engine_label_keeps_the_other_enrolled() {
    let b = fixture();
    on_github(&b, &issue(7, "sam", &["codex"]));
    let job = Job::new(&b.cfg, "j1");
    let _held = job.lock().unwrap();
    crate::turn::start(&b.cfg, "j1", "news", false).unwrap();
    assert_eq!(job.turns().as_deref(), Some("2"));
    assert!(b.path("launched").exists());
    assert!(!job.turn("1", "stopped").exists());
}

#[test]
fn unreadable_labels_defer_without_stopping_or_consuming_waits() {
    for response in [json!({}), json!({"labels": [{}]})] {
        let b = fixture();
        fs::write(b.path("repos_sam_box_issues_7"), response.to_string()).unwrap();
        let job = Job::new(&b.cfg, "j1");
        let before = fs::read(job.path("await")).unwrap();
        let _held = job.lock().unwrap();
        assert!(crate::turn::start(&b.cfg, "j1", "news", false).is_err());
        assert_eq!(job.turns().as_deref(), Some("1"));
        assert_eq!(fs::read(job.path("await")).unwrap(), before);
        assert!(!job.turn("1", "stopped").exists());
        assert!(!b.path("launched").exists());
    }
}

#[test]
fn legacy_inbox_jobs_also_check_labels_before_resuming() {
    let b = fixture();
    let job = Job::new(&b.cfg, "j1");
    fs::remove_file(job.path("inbox-issue")).unwrap();
    fs::remove_file(job.path("link")).unwrap();
    on_github(&b, &issue(7, "sam", &["later"]));
    let _held = job.lock().unwrap();
    assert!(crate::turn::start(&b.cfg, "j1", "news", false).is_err());
    assert_eq!(job.turns().as_deref(), Some("1"));
    assert!(job.turn("1", "stopped").exists());
    assert!(!job.path("await").exists());
}

#[test]
fn relabelling_after_a_refused_wake_retires_the_old_record() {
    let b = fixture();
    on_github(&b, &issue(7, "sam", &["later"]));
    let job = Job::new(&b.cfg, "j1");
    {
        let _held = job.lock().unwrap();
        assert!(crate::turn::start(&b.cfg, "j1", "news", false).is_err());
    }
    let ready = issue(7, "sam", &["ready", "codex"]);
    on_github(&b, &ready);
    b.pass().new_issues("sam/box", &[ready]).unwrap();
    let record = b.cfg.inbox.as_ref().unwrap().state.join("sam_box-7");
    assert_ne!(read(&record.join("job")).as_deref(), Some("j1"));
    assert_eq!(job.turns().as_deref(), Some("1"));
}

#[test]
fn a_repository_release_cannot_launch_a_parked_issues_queued_turn() {
    let b = fixture();
    let job = Job::new(&b.cfg, "j1");
    crate::policy::mark(&job, "queued", false).unwrap();
    on_github(&b, &issue(7, "sam", &["later"]));
    crate::policy::reconcile(&b.cfg).unwrap();
    assert!(!b.path("launched").exists());
    assert!(job.turn("1", "stopped").exists());
    assert!(!crate::policy::parked(&job));
    crate::policy::reconcile(&b.cfg).unwrap();
    assert!(
        !b.path("launched").exists(),
        "terminal parking must not retry a release"
    );
}

#[test]
fn an_explicit_stop_without_parking_does_not_enrol_a_replacement() {
    let b = fixture();
    crate::cmd::stop(&b.cfg, "j1", false).unwrap();
    let ready = issue(7, "sam", &["codex"]);
    on_github(&b, &ready);
    b.pass().new_issues("sam/box", &[ready]).unwrap();
    let record = b.cfg.inbox.as_ref().unwrap().state.join("sam_box-7");
    assert_eq!(read(&record.join("job")).as_deref(), Some("j1"));
    assert!(!b.path("launched").exists());
}

// A worker stopped by hand (`proj stop`) while its issue keeps its engine
// label: a reply isn't handed on, the issue hears why once, and no pass books
// a retry for it. Relabelling then starts a fresh job.
#[test]
fn a_hand_stopped_worker_tells_its_issue_once_and_never_retries() {
    let b = fixture();
    let d = b.record("j1");
    crate::cmd::stop(&b.cfg, "j1", false).unwrap();
    let enrolled = issue(7, "sam", &["codex"]);
    on_github(&b, &enrolled);
    fs::write(
        b.path("comments"),
        json!([[{"id": 9, "user": burrow_github::fake::actor("sam"), "body": "carry on",
                 "created_at": "2026-11-01T00:00:00Z"}]])
        .to_string(),
    )
    .unwrap();
    for _ in 0..2 {
        let mut p = b.pass();
        p.new_issues("sam/box", std::slice::from_ref(&enrolled))
            .unwrap();
        p.jobs("sam/box", "box", std::slice::from_ref(&enrolled))
            .unwrap();
        assert!(p.wake.is_empty(), "{:?}", p.wake);
    }
    let job = Job::new(&b.cfg, "j1");
    assert_eq!(job.turns().as_deref(), Some("1"));
    assert!(!b.path("launched").exists());
    assert_eq!(read(&d.join("job")).as_deref(), Some("j1"));
    let posted = b.posted();
    assert_eq!(posted.matches("was stopped").count(), 1, "{posted}");

    // The label off, then on again: a fresh job, not the stopped one.
    let unlabelled = issue(7, "sam", &[]);
    on_github(&b, &unlabelled);
    b.pass().jobs("sam/box", "box", &[]).unwrap();
    on_github(&b, &enrolled);
    b.pass().new_issues("sam/box", &[enrolled]).unwrap();
    let fresh = read(&d.join("job")).unwrap();
    assert_ne!(fresh, "j1");
    assert_eq!(Job::new(&b.cfg, &fresh).turns().as_deref(), Some("1"));
    assert_eq!(job.turns().as_deref(), Some("1"));
}
