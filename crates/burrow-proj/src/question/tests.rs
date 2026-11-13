use super::*;
use crate::{
    cmd::declaration_tests::response,
    config::{Config, tests::example},
};

struct Fixture {
    dir: tempfile::TempDir,
    cfg: Config,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg: Config = serde_json::from_value(example()).unwrap();
        cfg.jobs = dir.path().join("jobs");
        let script = dir.path().join("notify");
        burrow_test::executable(
            &script,
            format!(
                r#"#!/bin/sh
cd '{}'
printf '%s %s\n' "$1" "$2" >> calls
case "$1" in
ensure)
  touch "pending-$2"
  test -f fail-open && exit 0
  test -f "remote-$2" || printf '%s\n' "$4" >> deliveries
  touch "remote-$2" ;;
close)
  test -f fail-close && exit 0
  rm -f "remote-$2" "pending-$2" ;;
pending) test -f fail-pending && exit 2
  test -f "pending-$2" ;;
esac
"#,
                dir.path().display()
            ),
        );
        cfg.notify = Some(script);
        let job = Job::new(&cfg, "j1");
        fs::create_dir_all(&job.dir).unwrap();
        crate::policy::bind(
            &job,
            crate::policy::Identity::Local,
            crate::policy::Kind::Manual,
        )
        .unwrap();
        job.set_turns(1).unwrap();
        Self { dir, cfg }
    }
    fn job(&self) -> Job<'_> {
        Job::new(&self.cfg, "j1")
    }
    fn ending(&self, n: u64, how: &str) {
        let j = self.job();
        j.set_turns(n).unwrap();
        fs::write(j.turn(&n.to_string(), "end"), how).unwrap();
        j.mark_ended();
        fs::write(
            j.turn(&n.to_string(), "jsonl"),
            format!("{{\"type\":\"result\",\"result\":\"Question {n}?\",\"is_error\":false}}\n"),
        )
        .unwrap();
    }
    fn flag(&self, name: &str) {
        fs::write(self.dir.path().join(name), "").unwrap();
    }
    fn clear(&self, name: &str) {
        fs::remove_file(self.dir.path().join(name)).unwrap();
    }
    fn deliveries(&self) -> String {
        fs::read_to_string(self.dir.path().join("deliveries")).unwrap_or_default()
    }
}

#[test]
fn a_turn_keeps_the_answers_file_and_what_it_held_as_it_left_them() {
    let f = Fixture::new();
    f.ending(1, "ask");
    fs::write(f.job().path("answers.json"), "first").unwrap();
    fs::write(
        f.job().path("await"),
        "sam/r#5\t1800000000\t{}\nsam/up#3\t1800000000\t{}\n",
    )
    .unwrap();
    crate::cmd::finish(&f.cfg, "j1").unwrap();
    fs::write(f.job().path("answers.json"), "second").unwrap();
    // Kept once: a later capture (the next turn's start) doesn't take the
    // rewritten file for turn 1's.
    f.job().capture().unwrap();
    let kept = |name: &str| fs::read_to_string(f.job().turn("1", name)).unwrap();
    assert_eq!(kept("answers"), "first");
    assert_eq!(kept("holds"), "ask\nawait sam/r#5\nawait sam/up#3");
    assert!(f.job().turn("1", "captured").exists());
}

#[test]
fn a_turn_with_no_answers_is_confirmed_absent_not_assumed() {
    let f = Fixture::new();
    f.ending(1, "done");
    crate::cmd::finish(&f.cfg, "j1").unwrap();
    assert!(f.job().turn("1", "captured").exists());
    assert!(!f.job().turn("1", "answers").exists());
}

// An end hook that never ran (the machine went down) or failed leaves the
// capture to the next turn's start, which refuses to go on without it: the
// only copy of the answers is never rewritten uncollected.
#[test]
fn the_next_turn_does_not_start_until_the_last_ones_answers_are_kept() {
    let f = Fixture::new();
    f.ending(1, "done");
    // answers.json is a directory: it can't be read, which isn't absent.
    fs::create_dir(f.job().path("answers.json")).unwrap();
    assert!(crate::cmd::finish(&f.cfg, "j1").is_err());
    assert!(!f.job().turn("1", "captured").exists());
    let err = crate::turn::start(&f.cfg, "j1", "again", false).unwrap_err();
    assert!(err.contains("answers aren't kept"), "{err}");
    assert!(f.job().turns().as_deref() == Some("1"));
    // Readable again: kept by the start itself, without the end hook.
    fs::remove_dir(f.job().path("answers.json")).unwrap();
    fs::write(f.job().path("answers.json"), "left").unwrap();
    f.job().capture().unwrap();
    assert_eq!(
        fs::read_to_string(f.job().turn("1", "answers")).unwrap(),
        "left"
    );
}

#[test]
fn notifications_disabled_do_not_add_watcher_kicks() {
    use std::sync::{Arc, Mutex};
    let mut f = Fixture::new();
    f.cfg.notify = None;
    f.cfg.waits = None;
    let got = Arc::new(Mutex::new(Vec::new()));
    let seen = got.clone();
    let (port, _) = burrow_events::http::listen(
        0,
        Arc::new(move |req| {
            seen.lock().unwrap().push(req.target.clone());
            202
        }),
    )
    .unwrap();
    f.cfg.kick = format!("http://127.0.0.1:{port}/kick");
    f.cfg.tools.systemctl = "false".into();
    f.cfg.tools.systemd_run = "true".into();
    f.ending(1, "ask");
    crate::turn::start_answer(&f.cfg, "j1", "answer", false).unwrap();
    f.ending(2, "ask");
    fs::write(f.job().path("kick"), "completion").unwrap();
    crate::cmd::finish(&f.cfg, "j1").unwrap();
    f.cfg.tools.systemctl = "true".into();
    crate::cmd::stop(&f.cfg, "j1", false).unwrap();
    assert_eq!(*got.lock().unwrap(), ["/kick/completion"]);
}

#[test]
fn finalization_defers_instead_of_waiting_on_resume_lock() {
    use std::sync::{Arc, Mutex, mpsc};
    use std::time::Duration;
    let mut f = Fixture::new();
    f.cfg.wake = f.dir.path().join("wake.json");
    let got = Arc::new(Mutex::new(Vec::new()));
    let seen = got.clone();
    let (port, _) = burrow_events::http::listen(
        0,
        Arc::new(move |req| {
            seen.lock().unwrap().push(req.target.clone());
            202
        }),
    )
    .unwrap();
    f.cfg.kick = format!("http://127.0.0.1:{port}/kick");
    f.cfg.waits = None;
    f.ending(1, "ask");
    let held = f.job().lock().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            tx.send(crate::cmd::finish(&f.cfg, "j1").is_ok()).unwrap();
        });
        let completed = rx.recv_timeout(Duration::from_secs(5));
        drop(held);
        assert!(completed.unwrap());
    });
    assert!(got.lock().unwrap().iter().any(|s| s == "/kick/proj-watch"));
    assert!(f.deliveries().is_empty());
    crate::watch::look(&f.cfg, "").unwrap();
    assert!(f.deliveries().contains("Question 1?"));
}

#[test]
fn newer_marker_cannot_erase_an_older_failed_close_write() {
    let f = Fixture::new();
    f.ending(1, "ask");
    notify(&f.cfg, &f.job()).unwrap();
    Resume::prepare(&f.job(), true).unwrap().commit().unwrap();
    f.ending(2, "ask");
    fs::create_dir(f.job().turn("1", "question-close")).unwrap();
    assert!(notify(&f.cfg, &f.job()).is_err());
    assert_eq!(read(&f.job().path("open-question")).as_deref(), Some("2"));
    fs::remove_dir(f.job().turn("1", "question-close")).unwrap();
    notify(&f.cfg, &f.job()).unwrap();
    assert!(f.job().turn("1", "question-closed").exists());
    assert_eq!(f.deliveries().matches("Question 1?").count(), 1);
    assert!(f.dir.path().join("remote-proj-question-j1-2").exists());
}

#[test]
fn recovery_keeps_origin_across_automated_updates_and_unrelated_closes() {
    for legacy in [false, true] {
        let f = Fixture::new();
        f.ending(1, "ask");
        notify(&f.cfg, &f.job()).unwrap();
        f.flag("fail-close");
        Resume::prepare(&f.job(), true).unwrap().commit().unwrap();
        f.ending(2, "ask");
        fs::write(f.job().turn("1", "question-close"), "close").unwrap();
        fs::remove_file(f.job().path("question-answered-through")).unwrap();
        if legacy {
            fs::write(f.job().path("open-question"), "ask").unwrap();
        } else {
            fs::create_dir(f.job().turn("2", "question")).unwrap();
            assert!(notify(&f.cfg, &f.job()).is_err());
            fs::remove_dir(f.job().turn("2", "question")).unwrap();
        }
        Resume::prepare(&f.job(), false).unwrap().commit().unwrap();
        f.ending(3, "ask");
        notify(&f.cfg, &f.job()).unwrap();
        assert!(f.deliveries().contains("Question 2?"));
        assert!(!f.deliveries().contains("Question 3?"));
        assert_eq!(read(&f.job().path("open-question")).as_deref(), Some("2"));
    }
}

#[test]
fn legacy_recovery_excludes_previously_answered_questions() {
    for update in [OPEN_UPDATE, LEGACY_OPEN_UPDATE] {
        let f = Fixture::new();
        f.ending(1, "ask");
        f.ending(2, "ask");
        fs::write(
            f.job().path("prompt.2"),
            "A human answers the earlier question",
        )
        .unwrap();
        fs::write(f.job().path("prompt.3"), format!("event\n\n{update}\n")).unwrap();
        fs::write(f.job().path("open-question"), "ask").unwrap();
        f.ending(3, "ask");
        notify(&f.cfg, &f.job()).unwrap();
        assert!(f.deliveries().contains("Question 2?"));
        assert!(!f.deliveries().contains("Question 1?"));
        assert!(!f.deliveries().contains("Question 3?"));
    }
}

#[test]
fn orphaned_future_turns_cannot_answer_complete_or_replace_a_question() {
    for artifact in ["prompt", "done", "close", "closed", "replacement"] {
        for recorded in [false, true] {
            let f = Fixture::new();
            f.ending(1, "ask");
            if recorded {
                notify(&f.cfg, &f.job()).unwrap();
            }
            fs::write(f.job().path("open-question"), "ask").unwrap();
            match artifact {
                "prompt" => fs::write(f.job().path("prompt.2"), "partial human answer").unwrap(),
                "done" => fs::write(f.job().turn("2", "end"), "done").unwrap(),
                "close" => fs::write(f.job().turn("2", "question-close"), "close").unwrap(),
                "closed" => fs::write(f.job().turn("2", "question-closed"), "closed").unwrap(),
                _ => {
                    fs::write(f.job().turn("2", "end"), "ask").unwrap();
                    fs::write(f.job().turn("2", "ended"), "").unwrap();
                    fs::write(f.job().turn("2", "question"), "uncommitted question").unwrap();
                    fs::write(f.job().path("open-question"), "2").unwrap();
                }
            }
            notify(&f.cfg, &f.job()).unwrap();
            assert!(
                f.deliveries().contains("Question 1?"),
                "{artifact}, recorded={recorded}"
            );
            assert!(!f.deliveries().contains("uncommitted"));
            assert!(!f.job().turn("1", "question-close").exists());
            assert_eq!(
                f.job().turn("2", "question-close").exists(),
                artifact == "close"
            );
            assert_eq!(read(&f.job().path("open-question")).as_deref(), Some("1"));
        }
    }
}

#[test]
fn uncommitted_ask_cannot_create_a_question() {
    let f = Fixture::new();
    f.ending(1, "await");
    fs::write(f.job().turn("2", "end"), "ask").unwrap();
    fs::write(f.job().turn("2", "ended"), "").unwrap();
    fs::write(f.job().turn("2", "question"), "uncommitted question").unwrap();
    fs::write(f.job().path("open-question"), "2").unwrap();
    assert!(!notify(&f.cfg, &f.job()).unwrap());
    assert!(f.deliveries().is_empty());
}

#[test]
fn failed_prompt_writes_preserve_questions_for_human_and_automated_starts() {
    for human in [false, true] {
        let mut f = Fixture::new();
        f.cfg.tools.systemctl = "false".into();
        f.cfg.tools.systemd_run = "false".into();
        f.cfg.kick.clear();
        f.cfg.waits = None;
        f.ending(1, "ask");
        notify(&f.cfg, &f.job()).unwrap();
        let prompt = f.job().path("prompt.2");
        // An invalid destination makes atomic publication fail before start.
        // It stays quarantinable data, never a readable prompt or accepted reply.
        fs::create_dir(&prompt).unwrap();
        let question = Resume::prepare(&f.job(), human).unwrap();
        let result = crate::turn::write_prompt(&prompt, "partial prompt");
        assert!(result.is_err());
        drop(question);
        assert!(crate::state_reads::open(&prompt).is_err());
        assert_eq!(f.job().turns().as_deref(), Some("1"));
        assert_eq!(read(&f.job().path("open-question")).as_deref(), Some("1"));
        assert!(!f.job().path("question-answered-through").exists());
        notify(&f.cfg, &f.job()).unwrap();
        assert!(!f.job().turn("1", "question-close").exists());
        assert_eq!(f.deliveries().matches("Question 1?").count(), 1);
    }
}

#[test]
fn reused_numbers_cannot_commit_artifacts_from_failed_attempts() {
    for human in [false, true] {
        for succeeds in [false, true] {
            let mut f = Fixture::new();
            f.cfg.tools.systemctl = "false".into();
            f.cfg.tools.systemd_run = if succeeds { "true" } else { "false" }.into();
            f.cfg.kick.clear();
            f.cfg.waits = None;
            f.ending(1, "ask");
            notify(&f.cfg, &f.job()).unwrap();
            let artifacts = [
                "end",
                "outcome",
                "stopped",
                "ended",
                "question",
                "question-close",
                "question-closed",
                "jsonl",
                "slot",
                "wait",
            ];
            for t in ["2", "3"] {
                for kind in artifacts {
                    fs::write(
                        f.job().turn(t, kind),
                        if kind == "end" {
                            "done"
                        } else {
                            "stale attempt"
                        },
                    )
                    .unwrap();
                }
                fs::write(f.job().path(&format!("prompt.{t}")), "stale human answer").unwrap();
            }
            let start = || {
                if human {
                    crate::turn::start_answer(&f.cfg, "j1", "answer", false)
                } else {
                    crate::turn::start(&f.cfg, "j1", "event", false)
                }
            };
            assert_eq!(start().is_ok(), succeeds);
            for t in ["2", "3"] {
                for kind in artifacts {
                    assert!(!f.job().turn(t, kind).exists(), "{t}.{kind}");
                }
            }
            assert_eq!(
                f.job().turns().as_deref(),
                Some(if succeeds { "2" } else { "1" })
            );
            let attempts = fs::read_dir(f.job().path(".unstarted"))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            assert_eq!(attempts.len(), 1);
            assert_eq!(
                fs::read_to_string(attempts[0].path().join("2.end")).unwrap(),
                "done"
            );
            // Disk recovery after advancing the count must not accept the
            // old attempt's completion, stop or outbox as the new turn's.
            notify(&f.cfg, &f.job()).unwrap();
            assert_eq!(
                f.job().turn("1", "question-closed").exists(),
                human && succeeds
            );
            assert!(!f.deliveries().contains("stale attempt"));
            if succeeds {
                crate::turn::start(&f.cfg, "j1", "another event", false).unwrap();
                assert!(!f.job().turn("3", "end").exists());
                notify(&f.cfg, &f.job()).unwrap();
                assert_eq!(f.job().turn("1", "question-closed").exists(), human);
            }
        }
    }
}

#[test]
fn failed_quarantine_does_not_publish_a_turn_or_change_question_state() {
    let mut f = Fixture::new();
    f.cfg.tools.systemctl = "false".into();
    f.cfg.kick.clear();
    f.ending(1, "ask");
    notify(&f.cfg, &f.job()).unwrap();
    fs::write(f.job().turn("2", "end"), "done").unwrap();
    fs::write(f.job().path(".unstarted"), "blocked directory").unwrap();
    assert!(crate::turn::start_answer(&f.cfg, "j1", "answer", false).is_err());
    assert_eq!(f.job().turns().as_deref(), Some("1"));
    assert_eq!(read(&f.job().path("open-question")).as_deref(), Some("1"));
    assert!(!f.job().path("question-answered-through").exists());
    notify(&f.cfg, &f.job()).unwrap();
    assert!(!f.job().turn("1", "question-closed").exists());
}

#[test]
fn ordinary_jobs_do_not_add_question_watcher_kicks() {
    use std::sync::{Arc, Mutex};
    let mut f = Fixture::new();
    f.cfg.waits = None;
    let got = Arc::new(Mutex::new(Vec::new()));
    let seen = got.clone();
    let (port, _) = burrow_events::http::listen(
        0,
        Arc::new(move |req| {
            seen.lock().unwrap().push(req.target.clone());
            202
        }),
    )
    .unwrap();
    f.cfg.kick = format!("http://127.0.0.1:{port}/kick");
    f.cfg.tools.systemctl = "false".into();
    f.cfg.tools.systemd_run = "true".into();
    f.ending(1, "done");
    crate::turn::start(&f.cfg, "j1", "new work", false).unwrap();
    f.ending(2, "await");
    fs::write(f.job().path("kick"), "completion").unwrap();
    crate::cmd::finish(&f.cfg, "j1").unwrap();
    f.cfg.tools.systemctl = "true".into();
    crate::cmd::stop(&f.cfg, "j1", false).unwrap();
    assert_eq!(*got.lock().unwrap(), ["/kick/completion"]);
}

#[test]
fn failed_answer_cleanup_is_recovered_from_accepted_receipt() {
    let f = Fixture::new();
    f.ending(1, "ask");
    notify(&f.cfg, &f.job()).unwrap();
    fs::write(f.job().path("question-answered-through"), [0xff]).unwrap();
    assert!(Resume::prepare(&f.job(), true).is_err());
    assert_eq!(
        fs::read(f.job().path("question-answered-through")).unwrap(),
        [0xff]
    );
    assert!(notify(&f.cfg, &f.job()).is_err());
    fs::remove_file(f.job().path("question-answered-through")).unwrap();
    let pending = Resume::prepare(&f.job(), true).unwrap();
    fs::create_dir(f.job().path("question-answered-through")).unwrap();
    assert!(pending.commit().is_err());
    assert!(f.job().path("open-question").exists());
    fs::remove_dir(f.job().path("question-answered-through")).unwrap();
    f.job().set_turns(2).unwrap();
    notify(&f.cfg, &f.job()).unwrap();
    assert!(f.job().turn("1", "question-closed").exists());
    assert_eq!(
        read(&f.job().path("question-answered-through")).as_deref(),
        Some("1")
    );
}

#[test]
fn only_new_human_questions_reach_notify() {
    let f = Fixture::new();
    let j = f.job();
    for end in ["done", "await", "undeclared"] {
        f.ending(1, end);
        assert!(!notify(&f.cfg, &j).unwrap());
        assert!(f.deliveries().is_empty());
    }
    f.ending(1, "ask");
    notify(&f.cfg, &j).unwrap();
    assert!(f.deliveries().contains("Question 1?"));
    let notice = f.deliveries();
    notify(&f.cfg, &j).unwrap();
    Resume::prepare(&j, false).unwrap().commit().unwrap();
    notify(&f.cfg, &j).unwrap();
    assert_eq!(f.deliveries(), notice);
    drop(Resume::prepare(&j, true).unwrap());
    notify(&f.cfg, &j).unwrap();
    assert_eq!(f.deliveries(), notice);
    Resume::prepare(&j, true).unwrap().commit().unwrap();
    j.set_turns(2).unwrap();
    notify(&f.cfg, &j).unwrap();
    assert!(j.turn("1", "question-closed").exists());
}

#[test]
fn ambiguous_delivery_and_closure_survive_restarts_and_later_questions() {
    let f = Fixture::new();
    f.ending(1, "ask");
    f.flag("fail-open");
    notify(&f.cfg, &f.job()).unwrap(); // zero exit, but no delivery
    assert!(f.deliveries().is_empty());
    f.clear("fail-open");
    // State is on disk: a fresh Job retries the ambiguous operation.
    notify(&f.cfg, &f.job()).unwrap();
    notify(&f.cfg, &f.job()).unwrap();
    assert_eq!(f.deliveries().matches("Question 1?").count(), 1);
    f.flag("fail-close");
    Resume::prepare(&f.job(), true).unwrap().commit().unwrap();
    f.ending(2, "ask");
    notify(&f.cfg, &f.job()).unwrap();
    assert!(f.job().turn("1", "question-close").exists());
    assert!(!f.job().turn("1", "question-closed").exists());
    assert!(f.deliveries().contains("Question 2?"));
    f.clear("fail-close");
    notify(&f.cfg, &f.job()).unwrap();
    assert!(f.job().turn("1", "question-closed").exists());
    assert!(f.dir.path().join("remote-proj-question-j1-2").exists());
    assert_eq!(f.deliveries().matches("Question 2?").count(), 1);
}

#[test]
fn done_and_stopped_jobs_close_notices_without_the_question_marker() {
    for stopped in [false, true] {
        for native in [false, true] {
            let f = Fixture::new();
            f.ending(1, "ask");
            notify(&f.cfg, &f.job()).unwrap();
            f.ending(2, if stopped { "ask" } else { "done" });
            if stopped {
                fs::write(f.job().turn("2", "stopped"), "").unwrap();
            }
            if native {
                f.clear("jobs/j1/open-question");
            }
            notify(&f.cfg, &f.job()).unwrap();
            assert!(f.job().turn("1", "question-closed").exists());
            assert!(!f.job().turn("2", "question").exists());
            assert!(!notify(&f.cfg, &f.job()).unwrap());
        }
    }
}

#[test]
fn launch_nonzero_and_state_write_errors_retain_work() {
    let mut f = Fixture::new();
    f.ending(1, "ask");
    let notifier = f.cfg.notify.clone();
    f.cfg.notify = Some(f.dir.path().join("missing"));
    assert!(notify(&f.cfg, &f.job()).is_err());
    assert!(f.job().turn("1", "question").exists());
    f.cfg.notify = Some("false".into());
    assert!(notify(&f.cfg, &f.job()).is_err());
    f.cfg.notify = notifier;
    notify(&f.cfg, &f.job()).unwrap();
    assert!(f.deliveries().contains("Question 1?"));
    f.ending(2, "done");
    fs::create_dir(f.job().turn("1", "question-close")).unwrap();
    assert!(notify(&f.cfg, &f.job()).is_err());
    fs::remove_dir(f.job().turn("1", "question-close")).unwrap();
    notify(&f.cfg, &f.job()).unwrap();
    assert!(f.job().turn("1", "question-closed").exists());
}

#[test]
fn ambiguous_pending_and_large_answers_stay_recoverable() {
    let f = Fixture::new();
    f.ending(1, "ask");
    let stream =
        serde_json::json!({"type":"result", "result":"é".repeat(100_000), "is_error":false});
    fs::write(f.job().turn("1", "jsonl"), format!("{stream}\n")).unwrap();
    notify(&f.cfg, &f.job()).unwrap();
    assert!(f.deliveries().contains("Full answer: proj log"));
    assert!(f.deliveries().len() < 33_000);
    f.ending(2, "done");
    f.flag("fail-pending");
    notify(&f.cfg, &f.job()).unwrap();
    assert!(!f.job().turn("1", "question-closed").exists());
    f.clear("fail-pending");
    notify(&f.cfg, &f.job()).unwrap();
    assert!(f.job().turn("1", "question-closed").exists());
}

#[test]
fn finalization_runs_every_obligation_despite_notification_errors() {
    use std::sync::{Arc, Mutex};
    for failure in ["launch", "nonzero", "state"] {
        let mut f = Fixture::new();
        f.cfg.scratch = f.dir.path().join("scratch");
        f.cfg.waits = Some(f.dir.path().join("waits.json"));
        f.cfg.tools.systemctl = "false".into();
        let inbox = f.cfg.inbox.as_mut().unwrap();
        inbox.state = f.dir.path().join("inbox");
        fs::create_dir_all(&inbox.state).unwrap();
        fs::write(inbox.state.join(crate::inbox::WAITING), "sam/repo").unwrap();
        f.ending(1, "ask");
        fs::write(f.job().path("owned"), "").unwrap();
        fs::write(f.job().path("kick"), "completion").unwrap();
        let scratch = crate::scratch::dir(&f.cfg, "j1");
        fs::create_dir_all(&scratch).unwrap();
        fs::write(scratch.join("artifact"), "work").unwrap();
        match failure {
            "launch" => f.cfg.notify = Some(f.dir.path().join("absent")),
            "nonzero" => f.cfg.notify = Some("false".into()),
            _ => fs::create_dir(f.job().turn("1", "question")).unwrap(),
        }
        let got = Arc::new(Mutex::new(Vec::new()));
        let seen = got.clone();
        let (port, _) = burrow_events::http::listen(
            0,
            Arc::new(move |req| {
                seen.lock().unwrap().push(req.target.clone());
                202
            }),
        )
        .unwrap();
        f.cfg.kick = format!("http://127.0.0.1:{port}/kick");
        assert!(crate::cmd::finish(&f.cfg, "j1").is_err());
        assert!(f.job().turn("1", "ended").exists());
        assert!(f.cfg.waits.as_ref().unwrap().exists());
        assert!(!scratch.exists());
        let calls = got.lock().unwrap();
        for expected in [
            "/kick/completion",
            "/kick/proj-inbox/sam/repo",
            "/kick/proj-watch",
        ] {
            assert!(calls.iter().any(|s| s == expected), "{failure}: {calls:?}");
        }
    }
}

#[test]
fn watcher_retries_notices_without_github_waits_and_stop_closes_them() {
    let mut f = Fixture::new();
    f.cfg.kick.clear();
    f.cfg.waits = None;
    f.cfg.wake = f.dir.path().join("wake.json");
    f.cfg.tools.systemctl = "false".into();
    f.ending(1, "ask");
    f.flag("fail-open");
    crate::watch::look(&f.cfg, "").unwrap();
    assert!(f.deliveries().is_empty());
    let wake: serde_json::Value = serde_json::from_slice(&fs::read(&f.cfg.wake).unwrap()).unwrap();
    assert!(wake[""].as_i64().unwrap() > crate::now());
    f.clear("fail-open");
    crate::watch::look(&f.cfg, "").unwrap();
    assert!(f.deliveries().contains("Question 1?"));
    // A successful explicit stop goes through the same durable close.
    f.cfg.tools.systemctl = "true".into();
    crate::cmd::stop(&f.cfg, "j1", false).unwrap();
    assert!(f.job().turn("1", "question-closed").exists());
    crate::watch::look(&f.cfg, "").unwrap();
    assert_eq!(f.deliveries().matches("Question 1?").count(), 1);
}

#[test]
fn legacy_open_marker_does_not_acknowledge_delivery() {
    let f = Fixture::new();
    f.ending(1, "ask");
    Resume::prepare(&f.job(), false).unwrap().commit().unwrap();
    assert!(f.job().path("open-question").exists());
    assert!(!f.job().turn("1", "question").exists());
    f.job().set_turns(2).unwrap();
    notify(&f.cfg, &f.job()).unwrap();
    assert!(f.deliveries().contains("Question 1?"));
    notify(&f.cfg, &f.job()).unwrap();
    assert_eq!(f.deliveries().matches("Question 1?").count(), 1);
}

#[test]
fn initial_and_completed_questions_keep_normal_declarations() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg: Config = serde_json::from_value(example()).unwrap();
    cfg.jobs = dir.path().to_owned();
    let job = Job::new(&cfg, "j1");
    fs::create_dir_all(&job.dir).unwrap();
    crate::policy::bind(
        &job,
        crate::policy::Identity::Local,
        crate::policy::Kind::Manual,
    )
    .unwrap();
    let initial = Resume::prepare(&job, false).unwrap();
    assert_eq!(initial.prompt("new task"), "new task");
    initial.commit().unwrap();
    assert!(response(&cfg, "j1", "ask").contains("with your question"));
    job.set_turns(1).unwrap();
    fs::write(job.turn("1", "end"), "ask\n").unwrap();
    // A legacy asking job has no separate marker until its next turn.
    let retry = Resume::prepare(&job, false).unwrap();
    assert!(retry.prompt("update").contains("Do not repeat"));
    drop(retry);
    assert!(!job.path("open-question").exists());
    Resume::prepare(&job, false).unwrap().commit().unwrap();
    assert!(response(&cfg, "j1", "ask").contains("without repeating"));
    response(&cfg, "j1", "done");
    assert!(!job.path("open-question").exists());
    fs::write(job.turn("1", "end"), "done\n").unwrap();
    let next = Resume::prepare(&job, false).unwrap();
    assert_eq!(next.prompt("new task"), "new task");
    next.commit().unwrap();
    assert!(response(&cfg, "j1", "ask").contains("with your question"));
}

#[test]
fn notification_outbox_rejects_symlinked_bodies_and_boundaries() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    f.ending(1, "ask");
    let denied = f.dir.path().join("denied");
    fs::write(&denied, "ordinary confidential text").unwrap();
    symlink(&denied, f.job().turn("1", "question")).unwrap();
    assert!(notify(&f.cfg, &f.job()).is_err());
    assert!(!f.deliveries().contains("ordinary confidential text"));
    fs::remove_file(f.job().turn("1", "question")).unwrap();
    symlink(&denied, f.job().path("question-answered-through")).unwrap();
    assert!(notify(&f.cfg, &f.job()).is_err());
    assert!(Resume::prepare(&f.job(), true).is_err());
}

#[test]
fn machine_question_notifications_include_the_default_deadline_and_policy_once() {
    for one_way in [false, true] {
        let f = Fixture::new();
        f.ending(1, "ask");
        let job = f.job();
        fs::write(job.turn("1", "result"), "Question 1?").unwrap();
        let recommendation = crate::ask::Recommendation {
            default: "keep the old release".into(),
            one_way,
        };
        crate::ask::record(&job, &recommendation).unwrap();
        crate::ask::finish(&f.cfg, &job, "ask").unwrap();
        notify(&f.cfg, &job).unwrap();
        let body = f.deliveries();
        assert!(body.contains("Without an answer by"), "{body}");
        assert!(body.contains("keep the old release"), "{body}");
        assert!(
            body.contains(if one_way {
                "This is one-way"
            } else {
                "reversible default"
            }),
            "{body}"
        );
        Resume::prepare(&job, false).unwrap().commit().unwrap();
        f.ending(2, "ask");
        crate::ask::record(&job, &recommendation).unwrap();
        crate::ask::finish(&f.cfg, &job, "ask").unwrap();
        notify(&f.cfg, &job).unwrap();
        assert_eq!(f.deliveries().matches("Without an answer by").count(), 1);
        assert!(!f.deliveries().contains("Question 2?"));
    }
}

#[test]
fn interrupted_reply_preparations_preserve_question_and_outbox() {
    // Forget simulates SIGKILL/OOM/restart: no destructor or rollback runs.
    for source in ["operator", "inbox"] {
        for boundary in ["prepare", "prompt", "published"] {
            let f = Fixture::new();
            f.ending(1, "ask");
            notify(&f.cfg, &f.job()).unwrap();
            crate::ask::ensure(&f.job()).unwrap();
            for name in [
                "question-text",
                "question-since",
                "question-id",
                "question-needs-owner",
            ] {
                fs::write(crate::ask::host_path(&f.job(), name), "original authority").unwrap();
            }
            let pending = Resume::prepare(&f.job(), true).unwrap();
            std::mem::forget(pending);
            if boundary != "prepare" {
                crate::turn::write_prompt(&f.job().path("prompt.2"), source).unwrap();
            }
            if boundary == "published" {
                f.job().set_turns(2).unwrap();
            }
            // Fresh Job, as on the next watcher after the replying process dies.
            notify(&f.cfg, &f.job()).unwrap();
            assert_eq!(
                read(&f.job().path("open-question")).as_deref(),
                Some("1"),
                "{source}: {boundary}"
            );
            assert!(
                !f.job().turn("1", "question-close").exists(),
                "{source}: {boundary}"
            );
            assert!(!f.job().turn("1", "question-closed").exists());
            for name in [
                "question-text",
                "question-since",
                "question-id",
                "question-needs-owner",
            ] {
                assert_eq!(
                    read(&crate::ask::host_path(&f.job(), name)).as_deref(),
                    Some("original authority")
                );
            }
            assert!(f.dir.path().join("remote-proj-question-j1-1").exists());
        }
    }
}

#[test]
fn exec_accepts_reply_when_launcher_dies_before_commit() {
    let f = Fixture::new();
    f.ending(1, "ask");
    notify(&f.cfg, &f.job()).unwrap();
    let pending = Resume::prepare(&f.job(), true).unwrap();
    crate::turn::write_prompt(&f.job().path("prompt.2"), "human answer").unwrap();
    f.job().set_turns(2).unwrap();
    std::mem::forget(pending);
    // _exec proves that systemd accepted the reply, even without the launcher.
    accept(&f.job(), "2").unwrap();
    notify(&f.cfg, &f.job()).unwrap();
    assert!(f.job().turn("1", "question-closed").exists());
    // Re-entry is idempotent: it cannot delete the next question's state.
    fs::write(
        crate::ask::host_path(&f.job(), "question-text"),
        "next question",
    )
    .unwrap();
    accept(&f.job(), "2").unwrap();
    assert_eq!(
        read(&crate::ask::host_path(&f.job(), "question-text")).as_deref(),
        Some("next question")
    );
}

#[test]
fn accepted_reply_cleanup_recovers_after_every_interrupted_write() {
    // Receipt, boundary, each metadata deletion, marker deletion and final
    // applied receipt are distinct writes. Restart after every prefix.
    for prefix in 0..=METADATA.len() + 2 {
        let f = Fixture::new();
        f.ending(1, "ask");
        notify(&f.cfg, &f.job()).unwrap();
        crate::ask::ensure(&f.job()).unwrap();
        for name in METADATA {
            fs::write(crate::ask::host_path(&f.job(), name), "old question").unwrap();
        }
        let pending = Resume::prepare(&f.job(), true).unwrap();
        std::mem::forget(pending);
        f.job().set_turns(2).unwrap();
        save_receipt(
            &receipt_path(&f.job(), "2"),
            &Receipt {
                deadline: false,
                awaited: None,
                through: 1,
                answered: true,
                clear: true,
                accepted: true,
                applied: false,
            },
        )
        .unwrap();
        if prefix > 0 {
            fs::write(f.job().path("question-answered-through"), "1").unwrap();
        }
        for name in METADATA.iter().take(prefix.saturating_sub(1)) {
            fs::remove_file(crate::ask::host_path(&f.job(), name)).unwrap();
        }
        if prefix > METADATA.len() + 1 {
            fs::remove_file(f.job().path("open-question")).unwrap();
        }
        notify(&f.cfg, &f.job()).unwrap();
        assert!(
            f.job().turn("1", "question-closed").exists(),
            "prefix {prefix}"
        );
        for name in METADATA {
            assert!(
                !crate::ask::host_path(&f.job(), name).exists(),
                "{prefix}: {name}"
            );
        }
        notify(&f.cfg, &f.job()).unwrap();
        assert_eq!(f.deliveries().matches("Question 1?").count(), 1);
    }
}

mod recovery;

mod lifecycle;
