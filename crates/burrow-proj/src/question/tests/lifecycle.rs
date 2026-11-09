use super::*;

fn watched() -> Fixture {
    let mut f = Fixture::new();
    f.cfg.wake = f.dir.path().join("wake.json");
    f.cfg.waits = None;
    f.cfg.inbox = None;
    f.cfg.kick.clear();
    let control = f.dir.path().join("systemctl");
    burrow_test::executable(
        &control,
        format!(
            r#"#!/bin/sh
state=$(cat '{state}')
case "$*" in
*LoadState*) if test "$state" = absent; then echo not-found; else echo loaded; fi ;;
*Job*proj-ended@*) ;;
*Job*) cat '{pending}' ;;
*ActiveState*) if test "$state" = absent; then echo inactive; else echo "$state"; fi ;;
*is-active*) test "$state" = active || test "$state" = activating ;;
esac
"#,
            state = f.dir.path().join("unit-state").display(),
            pending = f.dir.path().join("unit-job").display()
        ),
    );
    f.cfg.tools.systemctl = control;
    f.cfg.tools.systemd_run = "true".into();
    fs::write(f.dir.path().join("unit-state"), "absent").unwrap();
    fs::write(f.dir.path().join("unit-job"), "").unwrap();
    f
}

fn question(f: &Fixture, one_way: bool, due: i64) -> String {
    f.ending(1, "ask");
    crate::ask::ensure(&f.job()).unwrap();
    let text = serde_json::json!({"recommendation":{"default":"original default", "one_way":one_way}, "due":due}).to_string();
    fs::write(crate::ask::host_path(&f.job(), "question"), &text).unwrap();
    fs::write(
        crate::ask::host_path(&f.job(), "question-id"),
        "original-id",
    )
    .unwrap();
    fs::write(
        crate::ask::host_path(&f.job(), "question-text"),
        "Original question?",
    )
    .unwrap();
    notify(&f.cfg, &f.job()).unwrap();
    text
}

fn publish(f: &Fixture, human: bool, failed_accept: bool) {
    let resume = Resume::prepare(&f.job(), human).unwrap();
    crate::turn::write_prompt(
        &f.job().path("prompt.2"),
        &resume.prompt("undelivered input"),
    )
    .unwrap();
    f.job().set_turns(2).unwrap();
    std::mem::forget(resume);
    if failed_accept {
        let blocked = PathBuf::from(format!("{}.new", receipt_path(&f.job(), "2").display()));
        fs::create_dir(&blocked).unwrap();
        assert!(accept(&f.job(), "2").is_err());
        fs::remove_dir(blocked).unwrap();
    }
}

#[test]
fn abandoned_preparations_restore_questions_and_their_original_deadlines() {
    for human in [false, true] {
        for one_way in [false, true] {
            for failed_accept in [false, true] {
                let f = watched();
                let due = crate::now() + 3600;
                let before = question(&f, one_way, due);
                publish(&f, human, failed_accept);
                crate::watch::look(&f.cfg, "").unwrap();
                assert_eq!(f.job().turns().as_deref(), Some("2"));
                assert_eq!(
                    read(&crate::ask::host_path(&f.job(), "question")).as_deref(),
                    Some(before.as_str())
                );
                assert_eq!(read(&f.job().path("open-question")).as_deref(), Some("1"));
                assert!(!f.job().turn("1", "question-close").exists());
                let receipt: Receipt =
                    serde_json::from_str(&read(&receipt_path(&f.job(), "2")).unwrap()).unwrap();
                assert!(!receipt.accepted && receipt.applied);
                assert_eq!(
                    read(&f.job().turn("2", "outcome")).as_deref(),
                    Some("failed")
                );
                assert!(
                    read(&f.job().path("prompt.2"))
                        .unwrap()
                        .contains("undelivered input")
                );
                assert!(
                    accept(&f.job(), "2").is_err(),
                    "abandoned intent cannot later run"
                );
                let wake: serde_json::Value =
                    serde_json::from_str(&fs::read_to_string(&f.cfg.wake).unwrap()).unwrap();
                assert!(wake[""].as_i64().is_some_and(|at| at <= due));
                // The restored deadline still produces the normal timeout
                // transition, rather than postponing it on every look.
                let expired = serde_json::json!({"recommendation":{"default":"original default", "one_way":one_way}, "due":crate::now()-1}).to_string();
                fs::write(crate::ask::host_path(&f.job(), "question"), expired).unwrap();
                crate::watch::look(&f.cfg, "").unwrap();
                assert_eq!(f.job().turns().as_deref(), Some("3"));
                assert!(crate::ask::load(&f.job()).is_none());
                if one_way {
                    assert!(crate::ask::host_path(&f.job(), "question-needs-owner").exists());
                    assert_eq!(read(&f.job().turn("3", "end")).as_deref(), Some("done"));
                } else {
                    assert!(
                        read(&f.job().path("prompt.3"))
                            .unwrap()
                            .contains("Take your reversible recommended answer")
                    );
                    assert!(f.job().turn("1", "question-closed").exists());
                }
            }
        }
    }
}

#[test]
fn active_launches_are_protected_until_acceptance_or_unit_exit() {
    for state in ["active", "activating", "deactivating", "unknown"] {
        for failed_accept in [false, true] {
            let f = watched();
            question(&f, false, crate::now() - 1);
            publish(&f, true, failed_accept);
            fs::write(f.dir.path().join("unit-state"), state).unwrap();
            crate::watch::look(&f.cfg, "").unwrap();
            assert_eq!(f.job().turns().as_deref(), Some("2"), "{state}");
            assert!(f.job().path("prompt.2").exists());
            assert!(!f.job().path("prompt.3").exists());
            assert!(!f.job().turn("1", "question-closed").exists());
            accept(&f.job(), "2").unwrap();
            fs::write(f.dir.path().join("unit-state"), "absent").unwrap();
            crate::watch::look(&f.cfg, "").unwrap();
            assert_eq!(f.job().turns().as_deref(), Some("2"));
            assert!(f.job().turn("1", "question-closed").exists());
        }
    }
}

#[test]
fn stopped_and_questionless_jobs_do_not_keep_recovery_retries() {
    for stopped in [false, true] {
        let f = watched();
        f.ending(1, "done");
        publish(&f, false, false);
        if stopped {
            crate::cmd::stop(&f.cfg, "j1", false).unwrap();
        }
        crate::watch::look(&f.cfg, "").unwrap();
        assert_eq!(f.job().turns().as_deref(), Some("2"));
        let wake: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&f.cfg.wake).unwrap()).unwrap();
        assert!(wake.as_object().unwrap().is_empty(), "{stopped}: {wake}");
        crate::watch::look(&f.cfg, "").unwrap();
        assert!(!acceptance_pending(&f.job()).unwrap());
    }
    // An initial start abandoned before systemd has no earlier turn to restore.
    let f = watched();
    fs::remove_file(f.job().path("turns")).unwrap();
    std::mem::forget(Resume::prepare(&f.job(), false).unwrap());
    fs::write(f.job().path("prompt.1"), "initial task").unwrap();
    f.job().set_turns(1).unwrap();
    crate::watch::look(&f.cfg, "").unwrap();
    assert_eq!(f.job().turns().as_deref(), Some("1"));
    assert_eq!(
        read(&f.job().turn("1", "outcome")).as_deref(),
        Some("failed")
    );
    let wake: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&f.cfg.wake).unwrap()).unwrap();
    assert!(wake.as_object().unwrap().is_empty());
}

#[test]
fn next_start_preserves_the_abandoned_turn_identity() {
    let f = watched();
    question(&f, false, crate::now() + 3600);
    publish(&f, true, false);
    crate::turn::start_answer(&f.cfg, "j1", "new answer", false).unwrap();
    assert_eq!(f.job().turns().as_deref(), Some("3"));
    assert!(
        read(&f.job().path("prompt.3"))
            .unwrap()
            .contains("new answer")
    );
    assert!(
        read(&f.job().path("prompt.2"))
            .unwrap()
            .contains("undelivered input")
    );
    assert_eq!(
        read(&f.job().turn("2", "outcome")).as_deref(),
        Some("failed")
    );
}

#[test]
fn queued_starts_and_unknown_jobs_preserve_replies_until_exec() {
    for state in ["inactive", "failed", "active", "unknown"] {
        for pending in [Some("42"), Some("42/start"), None] {
            for one_way in [false, true] {
                let f = watched();
                question(&f, one_way, crate::now() - 1);
                publish(&f, true, false);
                fs::write(f.dir.path().join("unit-state"), state).unwrap();
                let path = f.dir.path().join("unit-job");
                if let Some(value) = pending {
                    fs::write(path, value).unwrap();
                } else {
                    fs::remove_file(path).unwrap();
                }
                crate::watch::look(&f.cfg, "").unwrap();
                assert_eq!(f.job().turns().as_deref(), Some("2"), "{state} {pending:?}");
                assert!(acceptance_pending(&f.job()).unwrap());
                assert!(crate::ask::load(&f.job()).is_some());
                assert!(!crate::ask::host_path(&f.job(), "question-needs-owner").exists());
                assert!(!f.job().turn("1", "question-closed").exists());
                // The queued unit eventually enters _exec on this same receipt.
                accept(&f.job(), "2").unwrap();
                crate::watch::look(&f.cfg, "").unwrap();
                assert_eq!(f.job().turns().as_deref(), Some("2"));
                assert!(crate::ask::load(&f.job()).is_none());
                assert!(f.job().turn("1", "question-closed").exists());
            }
        }
    }
}
