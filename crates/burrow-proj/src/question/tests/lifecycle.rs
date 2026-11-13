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
    question_at(f, 1, one_way, due)
}

fn question_at(f: &Fixture, turn: u64, one_way: bool, due: i64) -> String {
    f.ending(turn, "ask");
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
                assert!(crate::ask::load(&f.job()).unwrap().is_none());
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
                assert!(crate::ask::load(&f.job()).unwrap().is_some());
                assert!(!crate::ask::host_path(&f.job(), "question-needs-owner").exists());
                assert!(!f.job().turn("1", "question-closed").exists());
                // The queued unit eventually enters _exec on this same receipt.
                accept(&f.job(), "2").unwrap();
                crate::watch::look(&f.cfg, "").unwrap();
                assert_eq!(f.job().turns().as_deref(), Some("2"));
                assert!(crate::ask::load(&f.job()).unwrap().is_none());
                assert!(f.job().turn("1", "question-closed").exists());
            }
        }
    }
}

fn paused_inbox(kind: &str) -> Fixture {
    let mut f = watched();
    let cfg: Config = serde_json::from_value(example()).unwrap();
    f.cfg.inbox = cfg.inbox;
    f.cfg.inbox.as_mut().unwrap().state = f.dir.path().join("inbox");
    f.cfg.settings.as_mut().unwrap().overlay = f.dir.path().join("overlay.json");
    f.cfg
        .settings
        .as_mut()
        .unwrap()
        .allowed
        .push("sam/r".into());
    crate::policy::bind(
        &f.job(),
        crate::policy::Identity::Repository("sam/r".into()),
        crate::policy::Kind::Automated,
    )
    .unwrap();
    f.cfg
        .settings
        .as_ref()
        .unwrap()
        .command(&["pause".into(), "sam/r".into()], "test")
        .unwrap();
    let root = &f.cfg.inbox.as_ref().unwrap().state;
    let record = root.join(if matches!(kind, "sorting" | "planner") {
        ".triage/sam_r-7"
    } else {
        "sam_r-7"
    });
    fs::create_dir_all(&record).unwrap();
    fs::write(record.join("job"), "j1").unwrap();
    if matches!(kind, "ordinary" | "drained") {
        fs::write(record.join("issue"), "sam/r#7").unwrap();
    }
    if kind == "drained" {
        fs::write(record.join("drained"), "sam/r#7").unwrap();
    }
    // Sorting records intentionally have no source metadata, as on old jobs.
    if kind == "planner" {
        fs::write(record.join("planner"), "1").unwrap();
    }
    let gh = f.dir.path().join("gh");
    burrow_test::executable(
        &gh,
        "#!/bin/sh\ncase \"$*\" in\n*comments*) printf 'HTTP/2.0 200 OK\\n\\n[]' ;;\n*) printf 'HTTP/2.0 200 OK\\n\\n{\"state\":\"open\",\"labels\":[{\"name\":\"claude\"}]}' ;;\nesac\n",
    );
    f.cfg.tools.gh = gh;
    f
}

#[test]
fn paused_inbox_kinds_preserve_all_automatic_waits() {
    for kind in ["ordinary", "drained", "sorting", "planner"] {
        for wait in ["github", "usage", "question"] {
            let f = paused_inbox(kind);
            if wait == "question" {
                question(&f, false, crate::now() - 10);
            } else {
                f.ending(1, "await");
                fs::write(f.job().turn("1", "engine"), "codex").unwrap();
                let text = if wait == "usage" {
                    format!("usage-limit\t1000\t{}\n", crate::now() - 1)
                } else {
                    format!(
                        "sam/r#7\t{}\t{{\"state\":\"closed\",\"comments\":0}}\n",
                        crate::now()
                    )
                };
                fs::write(f.job().path("await"), text).unwrap();
            }
            let before = read(&f.job().path("await"));
            crate::watch::look(&f.cfg, "").unwrap();
            assert_eq!(f.job().turns().as_deref(), Some("1"), "{kind}: {wait}");
            assert_eq!(read(&f.job().path("await")), before, "{kind}: {wait}");
            if wait == "question" {
                assert!(crate::ask::load(&f.job()).unwrap().is_some());
            }
            f.cfg
                .settings
                .as_ref()
                .unwrap()
                .command(&["allow".into(), "sam/r".into()], "test")
                .unwrap();
            crate::watch::look(&f.cfg, "").unwrap();
            assert_eq!(f.job().turns().as_deref(), Some("2"), "{kind}: {wait}");
            assert!(f.job().path("prompt.2").exists());
        }
    }
}

#[test]
fn paused_question_maintenance_retries_open_close_and_recovery() {
    for end in ["stop", "done", "answer"] {
        let f = paused_inbox("ordinary");
        f.flag("fail-open");
        question(&f, false, crate::now() + 3600);
        crate::watch::look(&f.cfg, "").unwrap();
        assert!(f.deliveries().is_empty());
        f.clear("fail-open");
        crate::watch::look(&f.cfg, "").unwrap();
        assert!(f.deliveries().contains("Question 1?"));

        // A failed human resume is recovered even while automatic turns pause.
        publish(&f, true, false);
        crate::watch::look(&f.cfg, "").unwrap();
        assert!(crate::ask::load(&f.job()).unwrap().is_some());
        assert_eq!(read(&f.job().path("open-question")).as_deref(), Some("1"));

        // Cleanup survives stop and failed durable notification delivery.
        fs::write(
            crate::ask::host_path(&f.job(), "label"),
            serde_json::json!({"repo":"sam/r","number":"7","name":f.cfg.waiting_owner_label})
                .to_string(),
        )
        .unwrap();
        f.flag("fail-close");
        match end {
            "stop" => crate::cmd::stop(&f.cfg, "j1", false).unwrap(),
            "done" => {
                f.ending(3, "done");
                crate::ask::finish(&f.cfg, &f.job(), "done").unwrap();
                crate::cmd::finish(&f.cfg, "j1").unwrap();
            }
            "answer" => crate::turn::start_answer(&f.cfg, "j1", "approved", false).unwrap(),
            _ => unreachable!(),
        }
        crate::watch::look(&f.cfg, "").unwrap();
        assert!(!crate::ask::host_path(&f.job(), "label").exists(), "{end}");
        assert!(!f.job().turn("1", "question-closed").exists());
        assert!(f.dir.path().join("remote-proj-question-j1-1").exists());
        f.clear("fail-close");
        crate::watch::look(&f.cfg, "").unwrap();
        assert!(f.job().turn("1", "question-closed").exists());
    }
}

#[test]
fn one_way_notices_survive_timeout_and_pause_until_answered_or_stopped() {
    for kind in ["ordinary", "drained", "sorting", "planner", "manual"] {
        for end in ["answer", "stop"] {
            let mut f = paused_inbox(kind);
            if kind == "manual" {
                f.cfg.inbox = None;
            }
            question(&f, true, crate::now() - 1);
            f.cfg
                .settings
                .as_ref()
                .unwrap()
                .command(&["allow".into(), "sam/r".into()], "test")
                .unwrap();
            f.flag("fail-open");
            crate::watch::look(&f.cfg, "").unwrap();
            assert!(crate::ask::host_path(&f.job(), "question-needs-owner").exists());
            assert_eq!(f.job().turns().as_deref(), Some("2"));
            assert!(
                !f.job().turn("1", "question-close").exists(),
                "{kind}: {end}"
            );
            f.clear("fail-open");
            for paused in [false, true, true, false, false] {
                f.cfg
                    .settings
                    .as_ref()
                    .unwrap()
                    .command(
                        &[
                            if paused {
                                "pause".into()
                            } else {
                                "allow".into()
                            },
                            "sam/r".into(),
                        ],
                        "test",
                    )
                    .unwrap();
                crate::watch::look(&f.cfg, "").unwrap();
                assert_eq!(f.job().turns().as_deref(), Some("2"));
                assert!(
                    !f.job().turn("1", "question-close").exists(),
                    "{kind}: {end}: {paused}"
                );
                assert!(f.dir.path().join("remote-proj-question-j1-1").exists());
            }
            // Failed human starts must not close the still-unanswered notice.
            let resume = Resume::prepare(&f.job(), true).unwrap();
            crate::turn::write_prompt(
                &f.job().path("prompt.3"),
                &resume.prompt("undelivered answer"),
            )
            .unwrap();
            f.job().set_turns(3).unwrap();
            std::mem::forget(resume);
            crate::watch::look(&f.cfg, "").unwrap();
            assert!(crate::ask::host_path(&f.job(), "question-needs-owner").exists());
            assert!(!f.job().turn("1", "question-close").exists());
            f.flag("fail-close");
            if end == "answer" {
                crate::turn::start_answer(&f.cfg, "j1", "owner decision", false).unwrap();
            } else {
                crate::cmd::stop(&f.cfg, "j1", false).unwrap();
            }
            crate::watch::look(&f.cfg, "").unwrap();
            assert!(!crate::ask::host_path(&f.job(), "question-needs-owner").exists());
            assert!(f.job().turn("1", "question-close").exists());
            assert!(!f.job().turn("1", "question-closed").exists());
            f.clear("fail-close");
            crate::watch::look(&f.cfg, "").unwrap();
            assert!(f.job().turn("1", "question-closed").exists());
        }
    }
}

#[test]
fn pending_owner_notice_does_not_block_an_older_close_or_its_own_open() {
    let f = watched();
    question(&f, false, crate::now() + 3600);
    f.flag("fail-close");
    f.ending(2, "done");
    crate::ask::finish(&f.cfg, &f.job(), "done").unwrap();
    crate::watch::look(&f.cfg, "").unwrap();
    assert!(!f.job().turn("1", "question-closed").exists());

    f.flag("fail-open");
    question_at(&f, 3, true, crate::now() - 1);
    crate::watch::look(&f.cfg, "").unwrap();
    assert!(crate::ask::host_path(&f.job(), "question-needs-owner").exists());
    assert!(!f.job().turn("3", "question-close").exists());
    assert!(!f.dir.path().join("remote-proj-question-j1-3").exists());
    f.clear("fail-open");
    f.clear("fail-close");
    crate::watch::look(&f.cfg, "").unwrap();
    assert!(f.job().turn("1", "question-closed").exists());
    assert!(!f.job().turn("3", "question-close").exists());
    assert!(f.dir.path().join("remote-proj-question-j1-3").exists());
}

fn closed_with(f: &Fixture) -> String {
    fs::read_to_string(f.dir.path().join("reasons")).unwrap_or_default()
}

fn recording(f: &Fixture) {
    let script = f.dir.path().join("notify");
    let old = fs::read_to_string(&script).unwrap();
    burrow_test::executable(
        &script,
        old.replacen(
            "case \"$1\" in",
            &format!(
                "test \"$1\" = close && printf '%s\\n' \"$3\" >> '{}'\ncase \"$1\" in",
                f.dir.path().join("reasons").display()
            ),
            1,
        ),
    );
}

#[test]
fn a_reversible_default_closes_its_notice_with_the_reason() {
    let f = watched();
    recording(&f);
    question(&f, false, crate::now() - 1);
    assert!(f.dir.path().join("remote-proj-question-j1-1").exists());
    crate::watch::look(&f.cfg, "").unwrap();
    assert_eq!(f.job().turns().as_deref(), Some("2"));
    crate::watch::look(&f.cfg, "").unwrap();
    assert!(!f.dir.path().join("remote-proj-question-j1-1").exists());
    assert!(f.job().turn("1", "question-closed").exists());
    assert!(
        closed_with(&f).contains("took its reversible default"),
        "{}",
        closed_with(&f)
    );
}

#[test]
fn a_one_way_question_keeps_its_notice_after_its_deadline() {
    let f = watched();
    recording(&f);
    question(&f, true, crate::now() - 1);
    crate::watch::look(&f.cfg, "").unwrap();
    crate::watch::look(&f.cfg, "").unwrap();
    assert!(f.dir.path().join("remote-proj-question-j1-1").exists());
    assert!(!f.job().turn("1", "question-close").exists());
    assert_eq!(closed_with(&f), "");
}

#[test]
fn each_question_keeps_its_own_closing_cause() {
    let f = watched();
    let job = f.job();
    fs::write(job.path("prompt.2"), "No answer by 12:00. impostor").unwrap();
    let receipt = |through, accepted, deadline| Receipt {
        through,
        answered: true,
        clear: true,
        deadline,
        awaited: None,
        accepted,
        applied: false,
        question: None,
    };
    // A person answered question 1 in turn 2; question 3's default ran in 4.
    save_receipt(&receipt_path(&job, "2"), &receipt(1, true, false)).unwrap();
    save_receipt(&receipt_path(&job, "4"), &receipt(3, true, true)).unwrap();
    assert!(close_reason(&job, "1", 4).contains("answered"));
    assert!(close_reason(&job, "3", 4).contains("deadline"));
    // A failed default start, its turn number reused by a person, is no timeout.
    save_receipt(&receipt_path(&job, "4"), &receipt(3, true, false)).unwrap();
    assert!(close_reason(&job, "3", 4).contains("answered"));
    // An unaccepted attempt answers nothing: the job's stop or end decides.
    save_receipt(&receipt_path(&job, "4"), &receipt(3, false, true)).unwrap();
    fs::write(job.turn("4", "stopped"), "").unwrap();
    assert!(close_reason(&job, "3", 4).contains("stopped"));
    fs::remove_file(job.turn("4", "stopped")).unwrap();
    fs::write(job.turn("4", "end"), "done\n").unwrap();
    assert!(close_reason(&job, "3", 4).contains("ended"));
}

#[test]
fn a_default_after_an_automatic_update_keeps_its_cause() {
    let f = watched();
    recording(&f);
    question(&f, false, crate::now() + 3600);
    // An automatic update resumes the job and keeps the question open.
    publish(&f, false, false);
    accept(&f.job(), "2").unwrap();
    f.ending(2, "done");
    fs::write(f.job().turn("2", "end"), "ask\n").unwrap();
    // Its deadline default is the next accepted answer.
    let ask = crate::ask::load(&f.job()).unwrap();
    assert!(ask.is_some());
    let resume = Resume::prepare_as(&f.job(), Answer::Deadline).unwrap();
    crate::turn::write_prompt(
        &f.job().path("prompt.3"),
        &resume.prompt("No answer by x. Take it"),
    )
    .unwrap();
    f.job().set_turns(3).unwrap();
    resume.commit().unwrap();
    crate::watch::look(&f.cfg, "").unwrap();
    assert!(f.job().turn("1", "question-closed").exists());
    assert!(
        closed_with(&f).contains("reversible default"),
        "{}",
        closed_with(&f)
    );
}

#[test]
fn a_reply_after_the_question_ended_does_not_answer_it() {
    let f = watched();
    let job = f.job();
    let receipt = |deadline| Receipt {
        through: 1,
        answered: true,
        clear: true,
        deadline,
        awaited: None,
        accepted: true,
        applied: true,
        question: None,
    };
    // Question 1 survived automatic turn 2, which completed the job; a
    // later human resume (receipt 3) is a new conversation, however its own
    // turn is going when the notice is finally sent.
    fs::write(job.turn("2", "end"), "done\n").unwrap();
    save_receipt(&receipt_path(&job, "3"), &receipt(false)).unwrap();
    assert!(close_reason(&job, "1", 3).contains("ended"));
    // An earlier accepted answer survives a later completion.
    save_receipt(&receipt_path(&job, "2"), &receipt(true)).unwrap();
    assert!(close_reason(&job, "1", 3).contains("deadline"));
    // A one-way timeout's synthetic completion does not end the question,
    // even once the owner state is gone: the human answer (receipt 3) counts
    // and its turn finishing before the notice does not turn it into "ended".
    fs::remove_file(receipt_path(&job, "2")).unwrap();
    fs::write(job.turn("2", "timeout"), "one-way\n").unwrap();
    fs::write(job.turn("3", "end"), "done\n").unwrap();
    assert!(close_reason(&job, "1", 3).contains("answered"));
    // The same history holds when the human turn is stopped.
    fs::write(job.turn("3", "stopped"), "").unwrap();
    assert!(close_reason(&job, "1", 3).contains("answered"));
    // A stop ends it too.
    fs::remove_file(receipt_path(&job, "3")).unwrap();
    fs::write(job.turn("2", "end"), "ask\n").unwrap();
    fs::remove_file(job.turn("2", "timeout")).unwrap();
    fs::write(job.turn("2", "stopped"), "").unwrap();
    assert!(close_reason(&job, "1", 3).contains("stopped"));
}

#[test]
fn a_delayed_notice_names_the_cause_recorded_in_history() {
    let f = watched();
    let job = f.job();
    // Question 1's automatic turn 2 ended in done; a new human turn 3 is
    // still running (no end yet) when the notice is built.
    fs::write(job.turn("2", "end"), "done\n").unwrap();
    assert!(close_reason(&job, "1", 3).contains("ended"));
}

#[test]
fn a_dead_jobs_notice_stays_closed_once_closed() {
    let mut f = watched();
    f.cfg.ask_timeout = 60;
    recording(&f);
    question(&f, false, crate::now() + 3600);
    // Asked before host records existed: nothing can ever answer it itself.
    fs::remove_file(crate::ask::host_path(&f.job(), "question")).unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    fs::File::options()
        .write(true)
        .open(f.job().turn("1", "end"))
        .unwrap()
        .set_modified(old)
        .unwrap();
    // Someone closes the notice by hand.
    fs::remove_file(f.dir.path().join("remote-proj-question-j1-1")).unwrap();
    fs::remove_file(f.dir.path().join("pending-proj-question-j1-1")).unwrap();
    let before = f.deliveries();
    crate::watch::look(&f.cfg, "").unwrap();
    crate::watch::look(&f.cfg, "").unwrap();
    assert_eq!(f.deliveries(), before, "no fresh notice was filed");
    assert!(!f.dir.path().join("remote-proj-question-j1-1").exists());
    assert!(f.job().turn("1", "question-closed").exists());
    assert!(
        closed_with(&f).contains("past its timeout"),
        "{}",
        closed_with(&f)
    );
}

#[test]
fn a_young_legacy_question_keeps_its_notice() {
    let mut f = watched();
    f.cfg.ask_timeout = 3600;
    question(&f, false, crate::now() + 3600);
    fs::remove_file(crate::ask::host_path(&f.job(), "question")).unwrap();
    crate::watch::look(&f.cfg, "").unwrap();
    assert!(f.dir.path().join("remote-proj-question-j1-1").exists());
    assert!(!f.job().turn("1", "question-lapsed").exists());
}

#[test]
fn unreadable_shared_authority_does_not_make_a_recorded_question_legacy() {
    let mut f = watched();
    f.cfg.ask_timeout = 60;
    question(&f, true, crate::now() - 1);
    crate::ask::record(
        &f.job(),
        &crate::ask::Recommendation {
            default: "original default".into(),
            one_way: true,
            about: Some("subject".into()),
        },
    )
    .unwrap();
    crate::ask::finish(&f.cfg, &f.job(), "ask").unwrap();
    let registry = crate::ask::control_root(&f.cfg).join(".about");
    fs::create_dir_all(&registry).unwrap();
    fs::write(registry.join("state.json"), "{damaged").unwrap();
    assert!(crate::ask::load(&f.job()).is_err());
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    fs::File::options()
        .write(true)
        .open(f.job().turn("1", "end"))
        .unwrap()
        .set_modified(old)
        .unwrap();
    // The error is the watcher's retry: the notice stays open meanwhile.
    assert!(notify(&f.cfg, &f.job()).is_err());
    assert!(!f.job().turn("1", "question-lapsed").exists());
    assert!(!f.job().turn("1", "question-closed").exists());
    assert!(f.dir.path().join("remote-proj-question-j1-1").exists());
}

/// Question 1 is open (reversible or one-way) and the job, resumed by an
/// automatic update, awaits o/r#5, which a stand-in gh says is merged now.
fn awaiting_a_merge(one_way: bool) -> Fixture {
    let mut f = watched();
    recording(&f);
    let gh = f.dir.path().join("gh");
    burrow_test::executable(
        &gh,
        r#"#!/bin/sh
for a; do ep=${a%%\?*}; done
printf 'HTTP/2.0 200 OK\n\n'
case $ep in
  */comments | */reviews) echo '[]' ;;
  */status) echo '{"total_count": 0, "state": "pending"}' ;;
  */check-runs) echo '{"check_runs": []}' ;;
  repos/o/r/issues/5) echo '{"pull_request": {}, "state": "closed"}' ;;
  repos/o/r/pulls/5) echo '{"state": "closed", "merged": true, "head": {"sha": "abc"}, "review_comments": 0}' ;;
  *) exit 1 ;;
esac
"#,
    );
    f.cfg.tools.gh = gh;
    question(&f, one_way, crate::now() + 3600);
    publish(&f, false, false);
    accept(&f.job(), "2").unwrap();
    f.ending(2, "await");
    fs::write(
        f.job().path("await"),
        format!(
            "o/r#5\t{}\t{}\n",
            crate::now(),
            r#"{"state":"open","merged":false,"head":"abc","comments":0,"review_comments":0,"reviews":{},"checks":""}"#
        ),
    )
    .unwrap();
    assert!(crate::ask::load(&f.job()).unwrap().is_some());
    crate::watch::look(&f.cfg, "").unwrap();
    assert_eq!(f.job().turns().as_deref(), Some("3"));
    crate::watch::look(&f.cfg, "").unwrap();
    f
}

#[test]
fn what_the_job_awaited_closing_settles_a_reversible_question() {
    let f = awaiting_a_merge(false);
    let prompt = read(&f.job().path("prompt.3")).unwrap();
    assert!(prompt.contains("o/r#5: "), "{prompt}");
    assert!(prompt.contains("Your open question is closed"), "{prompt}");
    assert!(!prompt.contains(OPEN_UPDATE), "{prompt}");
    assert!(crate::ask::load(&f.job()).unwrap().is_none());
    assert!(!f.dir.path().join("remote-proj-question-j1-1").exists());
    assert!(f.job().turn("1", "question-closed").exists());
    assert!(
        closed_with(&f).contains("what the job awaited closed (o/r#5 was merged)"),
        "{}",
        closed_with(&f)
    );
}

#[test]
fn what_the_job_awaited_closing_leaves_a_one_way_question_open() {
    let f = awaiting_a_merge(true);
    let prompt = read(&f.job().path("prompt.3")).unwrap();
    assert!(prompt.contains(OPEN_UPDATE), "{prompt}");
    assert!(crate::ask::load(&f.job()).unwrap().is_some());
    assert!(f.dir.path().join("remote-proj-question-j1-1").exists());
    assert!(!f.job().turn("1", "question-close").exists());
    assert_eq!(closed_with(&f), "");
}
