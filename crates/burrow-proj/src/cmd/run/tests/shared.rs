use super::super::{deliver, reply_answer};
use crate::ask::{
    self,
    about_tests::{asks, fixture},
    host_path,
};
use crate::{job::Job, read};
use std::fs;

#[test]
fn accepted_unreconciled_parked_answers_are_recovered_before_identity_validation() {
    for blocked in [false, true] {
        let (_dir, cfg) = fixture();
        for id in ["a", "z"] {
            asks(&cfg, id, Some("subject"), false);
        }
        let z = Job::new(&cfg, "z");
        let question = ask::cohort(&Job::new(&cfg, "a")).unwrap()[0].1.clone();
        // The launcher accepted a parked answer, but died before cleaning up.
        z.set_turns(2).unwrap();
        fs::write(z.path("prompt.2"), "first accepted answer").unwrap();
        crate::policy::mark(&z, "queued", false).unwrap();
        fs::write(
            host_path(&z, "resume-2"),
            r#"{"through":1,"answered":true,"clear":true,"accepted":true,"applied":false}"#,
        )
        .unwrap();
        if blocked {
            fs::remove_file(host_path(&z, "question-text")).unwrap();
            fs::create_dir(host_path(&z, "question-text")).unwrap();
            assert!(deliver(&cfg, "z", "subject", &question, "obsolete", false).is_err());
            fs::remove_dir(host_path(&z, "question-text")).unwrap();
        }
        deliver(&cfg, "z", "subject", &question, "obsolete", false).unwrap();
        assert_eq!(z.turns().as_deref(), Some("2"));
        assert_eq!(
            read(&z.path("prompt.2")).as_deref(),
            Some("first accepted answer")
        );
        assert!(ask::load(&z).unwrap().is_none());
        assert!(!z.path("prompt.3").exists());
    }
}

#[test]
fn a_fixed_cohort_receives_one_answer_even_when_every_follower_is_parked() {
    let (_dir, cfg) = fixture();
    for id in ["a", "b", "c", "d"] {
        asks(&cfg, id, Some("subject"), false);
        crate::policy::mark(&Job::new(&cfg, id), "done", false).unwrap();
    }
    let cohort = ask::cohort(&Job::new(&cfg, "a")).unwrap();
    reply_answer(&cfg, "a", "yes", false).unwrap();
    for (id, question) in cohort {
        deliver(&cfg, &id, "subject", &question, "duplicate", false).unwrap();
        let job = Job::new(&cfg, &id);
        assert_eq!(job.turns().as_deref(), Some("2"));
        assert!(read(&job.path("prompt.2")).unwrap().contains("yes"));
        assert!(!job.path("prompt.3").exists());
    }
}

#[test]
fn stale_cohort_entries_skip_stopped_answered_rekeyed_and_replaced_questions() {
    for change in ["stopped", "answered", "rekeyed", "replaced"] {
        let (_dir, cfg) = fixture();
        for id in ["a", "z"] {
            asks(&cfg, id, Some("subject"), false);
        }
        let z = Job::new(&cfg, "z");
        let question = ask::cohort(&Job::new(&cfg, "a")).unwrap()[0].1.clone();
        match change {
            "stopped" => crate::cmd::stop(&cfg, "z", false).unwrap(),
            "answered" => reply_answer(&cfg, "z", "other answer", false).unwrap(),
            "rekeyed" => {
                ask::merge(&cfg, "different", &["z".into()]).unwrap();
            }
            "replaced" => {
                ask::clear(&z);
                asks(&cfg, "z", Some("subject"), false);
            }
            _ => unreachable!(),
        }
        let before = z.turns();
        deliver(&cfg, "z", "subject", &question, "obsolete", false).unwrap();
        assert_eq!(z.turns(), before, "{change}");
    }
}

#[test]
fn a_live_unaccepted_resume_is_not_superseded_by_a_shared_answer() {
    let (_dir, cfg) = fixture();
    for id in ["a", "z"] {
        asks(&cfg, id, Some("subject"), false);
    }
    let z = Job::new(&cfg, "z");
    let question = ask::cohort(&Job::new(&cfg, "a")).unwrap()[0].1.clone();
    z.set_turns(2).unwrap();
    fs::write(z.path("prompt.2"), "in-flight answer").unwrap();
    fs::write(
        host_path(&z, "resume-2"),
        r#"{"through":1,"answered":true,"clear":true,"accepted":false,"applied":false}"#,
    )
    .unwrap();
    burrow_test::executable(
        &cfg.tools.systemctl,
        "#!/bin/sh\ncase \"$*\" in *LoadState*) echo loaded;; *ActiveState*) echo activating;; *Job*) echo 123;; esac\n",
    );
    deliver(&cfg, "z", "subject", &question, "obsolete", false).unwrap();
    assert_eq!(z.turns().as_deref(), Some("2"));
    assert_eq!(
        read(&z.path("prompt.2")).as_deref(),
        Some("in-flight answer")
    );
}
