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
fn repository_release_survives_label_errors_without_other_wakes() {
    for state in ["queued", "running"] {
        for unavailable in [false, true] {
            let b = fixture();
            let job = Job::new(&b.cfg, "j1");
            fs::remove_file(job.path("await")).unwrap();
            ask::clear(&job);
            crate::policy::mark(&job, state, false).unwrap();
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
