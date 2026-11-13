//! Parking must survive admission, refreshed snapshots and legacy retirement.
use super::parking_tests::{fixture, on_github};
use super::sorting_tests::issue;
use crate::{ask, job::Job, read};
use serde_json::{Value, json};
use std::fs;

fn listed(n: u64, labels: &[&str]) -> Value {
    let mut value = issue(n, "sam", labels);
    value["repository_url"] = json!("https://api.github.com/repos/sam/box");
    value["created_at"] = json!("2026-01-01T00:00:00Z");
    value["state"] = json!("open");
    value
}

#[test]
fn stale_lists_cannot_restore_parking_recorded_by_resume_admission() {
    for resume in ["wake", "deadline", "policy"] {
        for labels in [vec!["claude", "later"], vec![]] {
            let b = fixture();
            let snapshot = listed(7, &["claude"]);
            let old = Job::new(&b.cfg, "j1");
            on_github(&b, &listed(7, &labels));
            match resume {
                "wake" => {
                    on_github(&b, &listed(8, &["claude"]));
                    crate::watch::look(&b.cfg, "sam/box").unwrap();
                }
                "deadline" => {
                    let q = ask::Question {
                        recommendation: ask::Recommendation {
                            default: "continue".into(),
                            one_way: false,
                            about: None,
                        },
                        due: crate::now() - 1,
                        id: None,
                        pauses: Default::default(),
                    };
                    fs::write(ask::host_path(&old, "question"), json!(q).to_string()).unwrap();
                    let _held = old.lock().unwrap();
                    assert!(ask::expire(&b.cfg, &old).is_err());
                }
                _ => {
                    crate::policy::mark(&old, "queued", false, None).unwrap();
                    crate::policy::reconcile(&b.cfg).unwrap();
                }
            }
            let inbox = b.cfg.inbox.as_ref().unwrap();
            let exclusion = inbox.state.join(".parked/sam_box/7");
            assert!(old.turn("1", "stopped").exists());
            assert!(exclusion.exists());
            let mut pass = b.pass();
            pass.new_issues("sam/box", std::slice::from_ref(&snapshot))
                .unwrap();
            assert!(!b.path("launched").exists(), "{resume}: {labels:?}");
            assert!(exclusion.exists());
            // An unreadable restoration must also retain the exclusion and retry.
            fs::write(b.path("repos_sam_box_issues_7"), "{}").unwrap();
            pass.new_issues("sam/box", std::slice::from_ref(&snapshot))
                .unwrap();
            assert!(exclusion.exists());
            assert!(!b.path("launched").exists());
            assert!(pass.wake.contains_key("sam/box"));
            on_github(&b, &listed(7, &["ready", "codex"]));
            fs::write(b.path("fail"), "api repos/sam/box/issues/7").unwrap();
            pass.new_issues("sam/box", std::slice::from_ref(&snapshot))
                .unwrap();
            assert!(exclusion.exists());
            assert!(!b.path("launched").exists());
            fs::remove_file(b.path("fail")).unwrap();
            pass.new_issues("sam/box", std::slice::from_ref(&snapshot))
                .unwrap();
            let record = inbox.state.join("sam_box-7/job");
            let fresh = read(&record).unwrap();
            assert_ne!(fresh, "j1");
            assert!(!exclusion.exists());
            pass.new_issues("sam/box", &[snapshot]).unwrap();
            assert_eq!(read(&record).as_deref(), Some(fresh.as_str()));
            assert_eq!(old.turns().as_deref(), Some("1"));
        }
    }
}

#[test]
fn initial_admission_checks_live_enrollment_before_starting_a_worker() {
    for labels in [
        json!([{"name":"claude"},{"name":"later"}]),
        json!([]),
        Value::Null,
    ] {
        let b = fixture();
        let snapshot = listed(9, &["claude"]);
        let mut current = snapshot.clone();
        current["labels"] = labels;
        on_github(&b, &current);
        let mut pass = b.pass();
        pass.new_issues("sam/box", &[snapshot]).unwrap();
        assert!(!b.path("launched").exists());
        assert!(
            !b.cfg
                .inbox
                .as_ref()
                .unwrap()
                .state
                .join("sam_box-9/job")
                .exists()
        );
        assert!(pass.wake.contains_key("sam/box"));
    }
}

#[test]
fn parking_cannot_be_overwritten_by_an_admission_label_read_already_in_flight() {
    use std::{io::Read, sync::mpsc, thread, time::Duration};
    for labels in [vec!["claude", "later"], vec![]] {
        let b = fixture();
        let snapshot = listed(7, &["claude"]);
        on_github(&b, &snapshot);
        fs::rename(&b.cfg.tools.gh, b.path("original-gh")).unwrap();
        for name in ["captured", "release"] {
            rustix::fs::mkfifoat(
                rustix::fs::CWD,
                b.path(name),
                rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
            )
            .unwrap();
        }
        // Hold a successful enrollment response in flight while a wake tries
        // to record a newer park. These barriers control the request ordering.
        burrow_test::executable(
            &b.cfg.tools.gh,
            format!(
                r#"#!/bin/sh
if [ "$4 $5" = 'GET repos/sam/box/issues/7' ] && mkdir '{root}/first-read' 2>/dev/null; then
  printf 'HTTP/2.0 200 OK\n\n'
  cat '{root}/repos_sam_box_issues_7'
  echo captured >'{root}/captured'
  read -r release <'{root}/release'
else
  exec '{root}/original-gh' "$@"
fi
"#,
                root = b.path("").display()
            ),
        );
        thread::scope(|scope| {
            let start = scope.spawn(|| b.pass().start("sam/box", &snapshot, &[]));
            let mut captured = String::new();
            fs::File::open(b.path("captured"))
                .unwrap()
                .read_to_string(&mut captured)
                .unwrap();
            on_github(&b, &listed(7, &labels));
            let (sent, received) = mpsc::channel();
            let (trying, attempted) = mpsc::channel();
            let cfg = &b.cfg;
            let park = scope.spawn(move || {
                let job = Job::new(cfg, "j1");
                let _held = job.lock().unwrap();
                trying.send(()).unwrap();
                let refused = crate::turn::start(cfg, "j1", "await changed", false).is_err();
                sent.send(refused).unwrap();
            });
            attempted.recv().unwrap();
            let premature = received.recv_timeout(Duration::from_millis(100));
            // Always release the subprocess before asserting, even on failure.
            fs::write(b.path("release"), "continue\n").unwrap();
            start.join().unwrap().unwrap();
            park.join().unwrap();
            assert!(premature.is_err(), "parking raced the admission label read");
            assert!(received.recv().unwrap());
        });
        assert!(
            b.cfg
                .inbox
                .as_ref()
                .unwrap()
                .state
                .join(".parked/sam_box/7")
                .exists()
        );
        assert!(Job::new(&b.cfg, "j1").turn("1", "stopped").exists());
    }
}

#[test]
fn missing_session_replacement_rechecks_enrollment_after_retiring_the_old_worker() {
    for labels in [
        json!([{"name":"claude"},{"name":"later"}]),
        json!([]),
        Value::Null,
    ] {
        let b = fixture();
        let old = Job::new(&b.cfg, "j1");
        fs::remove_file(old.path("session")).unwrap();
        fs::write(
            b.path("comments"),
            json!([[{
                "id":42,"user":burrow_github::fake::actor("sam"),
                "body":"continue","created_at":"2020-01-01T00:00:00Z"
            }]])
            .to_string(),
        )
        .unwrap();
        let snapshot = listed(7, &["claude"]);
        on_github(&b, &snapshot);
        let mut current = snapshot.clone();
        current["labels"] = labels.clone();
        fs::write(b.path("parked-issue"), current.to_string()).unwrap();
        // Parking arrives after the final REST snapshot, as the old worker
        // is stopped for replacement. The cached task must not admit a turn.
        burrow_test::executable(
            &b.cfg.tools.systemctl,
            format!(
                r#"#!/bin/sh
if [ -e '{stopped}' ]; then cp '{root}/parked-issue' '{root}/repos_sam_box_issues_7'; fi
case "$*" in
*is-active*) exit 3;;
*LoadState*) echo not-found;;
*ActiveState*) echo inactive;;
esac
"#,
                root = b.path("").display(),
                stopped = old.turn("1", "stopped").display()
            ),
        );
        let mut pass = b.pass();
        pass.jobs("sam/box", "box", &[snapshot]).unwrap();
        assert!(!b.path("launched").exists(), "{labels}");
        assert!(old.turn("1", "stopped").exists());
        assert!(!old.path("await").exists());
        assert!(pass.wake.contains_key("sam/box"));
        let restored = listed(7, &["ready", "codex"]);
        // Subsequent stops no longer change the restored live labels.
        fs::write(b.path("parked-issue"), restored.to_string()).unwrap();
        on_github(&b, &restored);
        pass.jobs("sam/box", "box", std::slice::from_ref(&restored))
            .unwrap();
        pass.new_issues("sam/box", &[restored]).unwrap();
        let record = b.cfg.inbox.as_ref().unwrap().state.join("sam_box-7/job");
        assert_ne!(read(&record).as_deref(), Some("j1"));
        assert!(b.path("launched").exists());
    }
}

#[test]
fn retiring_a_worker_preserves_parking_against_both_automatic_admissions() {
    for labels in [vec!["ready"], vec![]] {
        let b = fixture();
        let parked = listed(7, &labels);
        on_github(&b, &parked);
        b.pass().jobs("sam/box", "box", &[]).unwrap();
        b.pass()
            .note_ready("sam/box", std::slice::from_ref(&parked))
            .unwrap();
        b.pass()
            .triage("sam/box", std::slice::from_ref(&parked))
            .unwrap();
        b.pass().drain().unwrap();
        assert!(
            !b.path("launched").exists(),
            "sorting must not restart parked work"
        );
        assert!(
            !b.log().contains("labels[]=codex"),
            "drain must not restore the engine label"
        );
        assert!(b.pass().drain_records().is_empty());

        // A new ready issue is still admitted; parking is per issue.
        let fresh = listed(8, &["ready"]);
        on_github(&b, &fresh);
        b.pass().note_ready("sam/box", &[parked, fresh]).unwrap();
        b.pass().drain().unwrap();
        assert!(b.log().contains("POST repos/sam/box/issues/8/labels"));

        // Explicit enrollment permits a fresh worker for the parked issue.
        let restored = listed(7, &["ready", "codex"]);
        on_github(&b, &restored);
        b.pass().new_issues("sam/box", &[restored]).unwrap();
        let record = b.cfg.inbox.as_ref().unwrap().state.join("sam_box-7");
        assert_ne!(read(&record.join("job")).as_deref(), Some("j1"));
        assert!(record.join("job").exists());
    }
}

#[test]
fn later_overrides_ready_in_both_cached_and_refreshed_drain_candidates() {
    for cached in [false, true] {
        let b = fixture();
        let parked = listed(8, &["ready", "later"]);
        let snapshot = if cached {
            listed(8, &["ready"])
        } else {
            parked.clone()
        };
        b.pass().note_ready("sam/box", &[snapshot]).unwrap();
        on_github(&b, &parked);
        b.pass().drain().unwrap();
        assert!(!b.log().contains("POST repos/sam/box/issues/8/labels"));
        assert!(
            b.pass().drain_records().is_empty(),
            "refused enrollment must not reserve capacity"
        );
    }
}

#[test]
fn a_refused_pending_enrollment_releases_drain_capacity_immediately() {
    let b = fixture();
    let dir = b.cfg.inbox.as_ref().unwrap().state.join("sam_box-8");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("drained"), "sam/box#8").unwrap();
    let parked = listed(8, &["ready", "later", "codex"]);
    on_github(&b, &parked);
    b.pass().note_ready("sam/box", &[parked]).unwrap();
    b.pass().drain().unwrap();
    assert!(!dir.exists());
    assert!(b.pass().drain_records().is_empty());
}

#[test]
fn final_refresh_governs_closure_replies_and_missing_session_fallback() {
    for action in ["close", "reply", "no-session"] {
        for labels in [
            json!([{"name":"claude"},{"name":"later"}]),
            json!([]),
            Value::Null,
        ] {
            let b = fixture();
            let job = Job::new(&b.cfg, "j1");
            let record = b.cfg.inbox.as_ref().unwrap().state.join("sam_box-7");
            fs::write(record.join("answered"), "1").unwrap();
            if action == "close" {
                fs::write(job.turn("1", "end"), "done").unwrap();
            } else {
                fs::write(
                    b.path("comments"),
                    json!([[{
                        "id":42,"user":burrow_github::fake::actor("sam"),
                        "body":"please continue","created_at":"2020-01-01T00:00:00Z"
                    }]])
                    .to_string(),
                )
                .unwrap();
            }
            if action == "no-session" {
                fs::remove_file(job.path("session")).unwrap();
            }
            let snapshot = listed(7, &["claude"]);
            let mut refreshed = snapshot.clone();
            refreshed["labels"] = labels.clone();
            on_github(&b, &refreshed);
            let mut pass = b.pass();
            pass.jobs("sam/box", "box", &[snapshot]).unwrap();
            assert!(!b.log().contains("state=closed"), "{action}: {labels}");
            assert!(!b.path("launched").exists(), "{action}: {labels}");
            assert_eq!(job.turns().as_deref(), Some("1"));
            if labels.is_null() {
                assert!(!job.turn("1", "stopped").exists());
                assert!(job.path("await").exists());
                assert!(pass.wake.contains_key("sam/box"));
            } else {
                assert!(job.turn("1", "stopped").exists());
                assert!(!job.path("await").exists());
            }
        }
    }
}

#[test]
fn every_legacy_worker_retirement_keeps_the_old_conversation_stopped() {
    for ending in [
        "closed",
        "done",
        "barred",
        "pending-retirement",
        "forgotten",
    ] {
        let b = fixture();
        let job = Job::new(&b.cfg, "j1");
        fs::remove_file(job.path("inbox-issue")).unwrap();
        fs::remove_file(job.path("link")).unwrap();
        let record = b.cfg.inbox.as_ref().unwrap().state.join("sam_box-7");
        fs::write(record.join("answered"), "1").unwrap();
        let active = listed(7, &["claude"]);
        on_github(&b, &active);
        match ending {
            "closed" => {
                let mut closed = active.clone();
                closed["state"] = json!("closed");
                fs::write(b.path("repos_sam_box_issues_7"), closed.to_string()).unwrap();
                b.pass().jobs("sam/box", "box", &[]).unwrap();
            }
            "done" => {
                fs::write(job.turn("1", "end"), "done").unwrap();
                b.pass()
                    .jobs("sam/box", "box", std::slice::from_ref(&active))
                    .unwrap();
            }
            "barred" => {
                b.pass()
                    .jobs("sam/box", "box", &[listed(7, &["claude", "outside text"])])
                    .unwrap();
            }
            "forgotten" => {
                crate::cmd::stop(&b.cfg, "j1", false).unwrap();
                super::forget(&b.cfg, &b.cfg.inbox.as_ref().unwrap().state, "sam/box").unwrap();
            }
            _ => {
                let root = b
                    .cfg
                    .inbox
                    .as_ref()
                    .unwrap()
                    .state
                    .join(".retired/sam_box/7");
                fs::create_dir_all(&root).unwrap();
                fs::rename(&record, root.join("j1")).unwrap();
                fs::write(job.turn("1", "stopped"), "").unwrap();
                // A direct reply can beat the inbox's next retirement pass.
            }
        }
        assert!(!record.exists(), "{ending}");
        on_github(&b, &active);
        b.pass().new_issues("sam/box", &[active]).unwrap();
        assert_ne!(read(&record.join("job")).as_deref(), Some("j1"));
        {
            let _held = job.lock().unwrap();
            assert!(
                crate::turn::start(&b.cfg, "j1", "late reply", false).is_err(),
                "{ending}"
            );
        }
        b.pass().retired("sam/box").unwrap();
        let _held = job.lock().unwrap();
        assert!(crate::turn::start(&b.cfg, "j1", "reply after retirement", false).is_err());
        assert_eq!(job.turns().as_deref(), Some("1"));
    }
}

#[test]
fn sorting_and_standalone_conversations_are_exempt_from_worker_labels() {
    for sorting in [false, true] {
        let b = fixture();
        let job = Job::new(&b.cfg, "j1");
        fs::remove_file(job.path("inbox-issue")).unwrap();
        fs::remove_file(job.path("link")).unwrap();
        let record = b.cfg.inbox.as_ref().unwrap().state.join("sam_box-7");
        if sorting {
            let root = b.cfg.inbox.as_ref().unwrap().state.join(".triage");
            fs::create_dir_all(&root).unwrap();
            fs::rename(record, root.join("sam_box-7")).unwrap();
        } else {
            fs::remove_dir_all(record).unwrap();
        }
        on_github(&b, &listed(7, &["later"]));
        let _held = job.lock().unwrap();
        crate::turn::start(&b.cfg, "j1", "ordinary resume", false).unwrap();
        assert!(b.path("launched").exists());
    }
}

#[test]
fn missing_session_replacement_retires_the_old_worker_and_its_await() {
    let b = fixture();
    let old = Job::new(&b.cfg, "j1");
    for path in ["inbox-issue", "link", "session"] {
        fs::remove_file(old.path(path)).unwrap();
    }
    fs::write(
        b.path("comments"),
        json!([[{
            "id":42,"user":burrow_github::fake::actor("sam"),
            "body":"continue","created_at":"2020-01-01T00:00:00Z"
        }]])
        .to_string(),
    )
    .unwrap();
    let active = listed(7, &["claude"]);
    on_github(&b, &active);
    b.pass().jobs("sam/box", "box", &[active]).unwrap();
    let record = b.cfg.inbox.as_ref().unwrap().state.join("sam_box-7");
    let fresh = read(&record.join("job")).unwrap();
    assert_ne!(fresh, "j1");
    assert_eq!(Job::new(&b.cfg, &fresh).turns().as_deref(), Some("1"));
    assert!(old.turn("1", "stopped").exists());
    assert!(!old.path("await").exists());
    let _held = old.lock().unwrap();
    assert!(crate::turn::start(&b.cfg, "j1", "late await", false).is_err());
}

#[test]
fn repository_release_survives_label_errors_without_other_wakes() {
    for state in ["queued", "running"] {
        for unavailable in [false, true] {
            let b = fixture();
            let job = Job::new(&b.cfg, "j1");
            fs::remove_file(job.path("await")).unwrap();
            ask::clear(&job);
            crate::policy::mark(&job, state, false, None).unwrap();
            fs::write(b.path("repos_sam_box_issues_7"), "{}").unwrap();
            if unavailable {
                fs::write(b.path("fail"), "api repos/sam/box/issues/7").unwrap();
            }
            assert!(crate::policy::reconcile(&b.cfg).is_err());
            assert!(crate::policy::parked(&job));
            assert!(!b.path("launched").exists());
            let generation = fs::read(b.cfg.jobs.join(".policy-generation")).unwrap();
            if unavailable {
                fs::remove_file(b.path("fail")).unwrap();
            }
            on_github(&b, &listed(7, &["claude"]));
            // The service's failure restart retries this unchanged generation.
            crate::policy::reconcile(&b.cfg).unwrap();
            assert_eq!(
                fs::read(b.cfg.jobs.join(".policy-generation")).unwrap(),
                generation
            );
            assert!(b.path("launched").exists());
            assert!(!crate::policy::parked(&job));
        }
    }
}
