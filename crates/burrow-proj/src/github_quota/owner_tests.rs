use super::tests::fixture;
use super::*;

fn job(cfg: &Config) -> Job<'_> {
    let job = Job::new(cfg, "owner");
    fs::create_dir_all(&job.dir).unwrap();
    crate::policy::bind(
        &job,
        crate::policy::Identity::Local,
        crate::policy::Kind::Automated,
    )
    .unwrap();
    job.set_turns(1).unwrap();
    job
}
fn ask(job: &Job, one_way: bool) {
    crate::ask::record(
        job,
        &crate::ask::Recommendation {
            default: "Keep the archive".into(),
            one_way,
            about: None,
        },
    )
    .unwrap();
    fs::write(job.path("end.next"), "ask").unwrap();
}
fn quota(job: &Job) {
    fs::write(
        job.path(PENDING),
        serde_json::to_vec(&Pending {
            targets: vec!["sam/r#7".into()],
            retry_at: 1,
            turn: job.turns().unwrap(),
            baselines: String::new(),
        })
        .unwrap(),
    )
    .unwrap();
    fs::write(job.turn(&job.turns().unwrap(), "outcome"), "failed").unwrap();
}

#[test]
fn active_one_way_upgrade_precedes_quota_recovery_and_expiry() {
    for recovery in ["finish", "replay", "restore", "watch"] {
        for past in [false, true] {
            let (_d, cfg) = fixture();
            let job = job(&cfg);
            fs::write(job.turn("1", "result"), "Which archive?").unwrap();
            ask(&job, false);
            crate::ask::finish(&cfg, &job, "ask").unwrap();
            let mut q = crate::ask::load(&job).unwrap().unwrap();
            q.due = now() + if past { -1 } else { 3600 };
            fs::write(
                crate::ask::host_path(&job, "question"),
                serde_json::to_vec(&q).unwrap(),
            )
            .unwrap();
            let id = read(&crate::ask::host_path(&job, "question-id"));
            ask(&job, true);
            quota(&job);
            match recovery {
                "finish" => {
                    finish(&job, "1").unwrap();
                }
                "replay" => {
                    replay(&cfg, &job).unwrap();
                }
                "restore" => {
                    restore(&job).unwrap();
                }
                _ => (),
            }
            crate::watch::look(&cfg, "").unwrap();
            if past {
                assert!(
                    crate::ask::host_path(&job, "question-needs-owner").exists(),
                    "old reversible deadline executed instead of retaining owner hold"
                );
                assert!(!job.path("prompt.2").exists());
            } else {
                assert!(
                    crate::ask::load(&job)
                        .unwrap()
                        .unwrap()
                        .recommendation
                        .one_way,
                    "lost active one-way upgrade"
                );
                assert_eq!(crate::ask::load(&job).unwrap().unwrap().due, q.due);
            }
            assert_eq!(read(&crate::ask::host_path(&job, "question-id")), id);
            assert_eq!(
                read(&crate::ask::host_path(&job, "question-text")).as_deref(),
                Some("Which archive?")
            );
        }
    }
}

#[test]
fn pending_recommendations_belong_to_their_turn_and_native_declaration() {
    let (_d, cfg) = fixture();
    let job = job(&cfg);
    ask(&job, true);
    job.set_turns(2).unwrap();
    quota(&job);
    finish(&job, "2").unwrap();
    assert!(
        crate::ask::load(&job).unwrap().is_none(),
        "old-turn ask was resurrected"
    );
    ask(&job, true);
    crate::cmd::declaration_tests::response(&cfg, job.id, "done");
    assert!(!job.path("ask.next").exists());
    ask(&job, true);
    crate::cmd::record_wait(
        &cfg,
        &job,
        &[("sam/r".into(), "7".into())],
        &["sam/r#7".into()],
        None,
    )
    .unwrap();
    assert!(!job.path("ask.next").exists());
}

#[test]
fn superseded_pending_asks_are_not_revived_by_quota() {
    for end in ["done", "await"] {
        let (_d, cfg) = fixture();
        let job = job(&cfg);
        ask(&job, true);
        fs::write(job.path("end.next"), end).unwrap();
        quota(&job);
        finish(&job, "1").unwrap();
        assert!(
            crate::ask::load(&job).unwrap().is_none(),
            "revived obsolete ask after {end}"
        );
        assert!(!job.path("ask.next").exists());
    }
}

#[test]
fn recovered_ask_is_visible_and_notified_in_its_first_pass_with_retries() {
    let (d, mut cfg) = fixture();
    let notify = d.path().join("notify");
    burrow_test::executable(
        &notify,
        format!(
            r#"#!/bin/sh
cd '{}'
echo "$1 $2" >> calls
test ! -e fail || exit 1
case "$1" in
ensure) test -e "$2" || echo "$2" >> delivered; touch "$2" ;;
pending) test -e "$2" ;;
esac
"#,
            d.path().display()
        ),
    );
    cfg.notify = Some(notify);
    let job = job(&cfg);
    crate::ask::initialize(&job, Some("sam/r#7")).unwrap();
    ask(&job, true);
    fs::remove_file(job.path("end.next")).unwrap();
    quota(&job);
    fs::write(d.path().join("fail"), "").unwrap();
    crate::watch::look(&cfg, "").unwrap();
    assert!(
        read(&d.path().join("calls")).is_some_and(|s| s.contains("ensure proj-question-owner-1")),
        "recovered ask was not offered to notifier"
    );
    assert!(
        crate::ask::host_path(&job, "label").exists(),
        "recovered ask was not made visible"
    );
    let id = read(&crate::ask::host_path(&job, "question-id"));
    let wake: serde_json::Value = serde_json::from_str(&read(&cfg.wake).unwrap()).unwrap();
    assert!(wake[""].as_i64().unwrap() < crate::ask::load(&job).unwrap().unwrap().due);
    // A restart after committing authority but before publishing its turn
    // markers must recover the same outbox operation, not create a new ask.
    fs::remove_file(job.turn("1", "end")).unwrap();
    fs::remove_file(job.turn("1", "outcome")).unwrap();
    fs::remove_file(d.path().join("fail")).unwrap();
    for _ in 0..2 {
        crate::watch::look(&cfg, "").unwrap();
    }
    assert_eq!(read(&crate::ask::host_path(&job, "question-id")), id);
    assert_eq!(
        read(&d.path().join("delivered")).unwrap().lines().count(),
        1
    );
}
