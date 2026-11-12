use super::*;

pub(crate) fn fixture() -> (tempfile::TempDir, Config) {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
    cfg.jobs = dir.path().join("jobs");
    cfg.scratch = dir.path().join("scratch");
    cfg.tools.systemctl = dir.path().join("systemctl");
    burrow_test::executable(
        &cfg.tools.systemctl,
        "#!/bin/sh\ncase \"$*\" in *is-active*) exit 3 ;; *LoadState*) echo not-found ;; esac\n",
    );
    cfg.tools.systemd_run = "true".into();
    cfg.kick = "http://127.0.0.1:1/kick".into();
    let notify = dir.path().join("notify");
    burrow_test::executable(
        &notify,
        format!(
            "#!/bin/sh\nprintf '%s|%s|%s\\n' \"$1\" \"$2\" \"$4\" >> '{}'\n",
            dir.path().join("calls").display()
        ),
    );
    cfg.notify = Some(notify);
    (dir, cfg)
}

/// The job's turn 1 ends asking `key`, as proj does when the turn finishes.
pub(crate) fn asks(cfg: &Config, id: &str, key: Option<&str>, one_way: bool) {
    let job = Job::new(cfg, id);
    fs::create_dir_all(&job.dir).unwrap();
    crate::policy::bind(
        &job,
        crate::policy::Identity::Local,
        crate::policy::Kind::Manual,
    )
    .unwrap();
    job.set_turns(1).unwrap();
    fs::write(job.path("session"), "s1\n").unwrap();
    fs::write(job.turn("1", "end"), "ask\n").unwrap();
    fs::write(job.turn("1", "outcome"), "done\n").unwrap();
    fs::write(
        job.turn("1", "result"),
        format!("May {id} edit the profile?"),
    )
    .unwrap();
    fs::write(
        job.turn("1", "jsonl"),
        format!(
            "{{\"type\":\"result\",\"result\":\"May {id} edit the profile?\",\"is_error\":false}}\n"
        ),
    )
    .unwrap();
    record(
        &job,
        &Recommendation {
            default: "leave it".into(),
            one_way,
            about: key.map(|k| about::normalize(k).unwrap()),
        },
    )
    .unwrap();
    finish(cfg, &job, "ask").unwrap();
}

pub(crate) fn calls(dir: &tempfile::TempDir) -> String {
    fs::read_to_string(dir.path().join("calls")).unwrap_or_default()
}

#[test]
fn two_jobs_asking_about_one_key_post_one_notice_and_one_reply_resumes_both() {
    let (dir, cfg) = fixture();
    asks(&cfg, "j1", Some("Edit the  README profile"), false);
    asks(&cfg, "j2", Some("edit the readme profile"), false);
    for id in ["j1", "j2"] {
        let job = Job::new(&cfg, id);
        let _held = job.lock().unwrap();
        crate::question::notify(&cfg, &job).unwrap();
    }
    let seen = calls(&dir);
    assert_eq!(seen.matches("ensure|").count(), 2 - 1, "{seen}");
    assert!(seen.contains("proj-question-j1-1"), "{seen}");
    assert!(
        seen.contains("close|proj-question-j2"),
        "closure is confirmed even without delivery metadata: {seen}"
    );
    assert!(
        seen.contains("waiting on this answer for 1 other job: j2"),
        "{seen}"
    );
    // The owner's digest is one line, naming who else waits on it.
    let lines = owner_lines(&cfg);
    assert_eq!(lines.len(), 1);
    assert!(lines[0].2.contains("also waiting on it: j2"), "{lines:?}");
    // The follower waits on the leader's deadline.
    assert_eq!(
        load(&Job::new(&cfg, "j1")).unwrap().due,
        load(&Job::new(&cfg, "j2")).unwrap().due
    );
    crate::cmd::reply_answer(&cfg, "j1", "yes, edit it", false).unwrap();
    for id in ["j1", "j2"] {
        let job = Job::new(&cfg, id);
        assert_eq!(job.turns().as_deref(), Some("2"), "{id}");
        assert!(
            read(&job.path("prompt.2"))
                .unwrap()
                .contains("yes, edit it")
        );
        assert!(load(&job).is_none(), "{id}");
    }
}

#[test]
fn different_keys_and_no_key_each_keep_their_own_notice() {
    let (dir, cfg) = fixture();
    asks(&cfg, "j1", Some("profile"), false);
    asks(&cfg, "j2", Some("release"), false);
    asks(&cfg, "j3", None, false);
    for id in ["j1", "j2", "j3"] {
        let job = Job::new(&cfg, id);
        let _held = job.lock().unwrap();
        crate::question::notify(&cfg, &job).unwrap();
    }
    assert_eq!(calls(&dir).matches("ensure|").count(), 3);
    assert_eq!(owner_lines(&cfg).len(), 3);
}

#[test]
fn a_follower_takes_the_notice_over_when_its_leader_stops() {
    let (dir, cfg) = fixture();
    asks(&cfg, "j1", Some("profile"), false);
    asks(&cfg, "j2", Some("profile"), false);
    crate::cmd::stop(&cfg, "j1", false).unwrap();
    let job = Job::new(&cfg, "j2");
    let _held = job.lock().unwrap();
    crate::question::notify(&cfg, &job).unwrap();
    assert!(calls(&dir).contains("ensure|proj-question-j2-1"));
}

#[test]
fn one_way_leader_makes_the_follower_one_way() {
    let (_dir, cfg) = fixture();
    asks(&cfg, "j1", Some("profile"), true);
    asks(&cfg, "j2", Some("profile"), false);
    assert!(load(&Job::new(&cfg, "j2")).unwrap().recommendation.one_way);
}

#[test]
fn merge_collapses_existing_duplicates_into_the_newest_notice() {
    let (dir, cfg) = fixture();
    for id in ["j1", "j2", "j3"] {
        asks(&cfg, id, None, false);
        let job = Job::new(&cfg, id);
        let _held = job.lock().unwrap();
        crate::question::notify(&cfg, &job).unwrap();
    }
    // Newer since-stamps lead; make j2 the newest.
    for (id, since) in [("j1", "100"), ("j2", "300"), ("j3", "200")] {
        fs::write(host_path(&Job::new(&cfg, id), "question-since"), since).unwrap();
    }
    assert_eq!(calls(&dir).matches("ensure|").count(), 3);
    let ids: Vec<String> = ["j1", "j2", "j3"].map(String::from).into();
    assert_eq!(merge(&cfg, "Profile edit", &ids).unwrap(), "j2");
    for id in ["j1", "j2", "j3"] {
        let job = Job::new(&cfg, id);
        let _held = job.lock().unwrap();
        crate::question::notify(&cfg, &job).unwrap();
    }
    let seen = calls(&dir);
    assert!(seen.contains("close|proj-question-j1-1|"), "{seen}");
    assert!(seen.contains("close|proj-question-j3-1|"), "{seen}");
    assert!(!seen.contains("close|proj-question-j2"), "{seen}");
    assert_eq!(owner_lines(&cfg).len(), 1);
    assert!(
        merge(&cfg, "x", &["nope".to_owned()]).is_err(),
        "an unknown job is refused"
    );
}

#[test]
fn keys_are_short_single_lines() {
    assert!(about::normalize("  ").is_err());
    assert!(about::normalize("a\u{7}b").is_err());
    assert!(about::normalize(&"x".repeat(121)).is_err());
    assert!(Recommendation::parse(&["--default".into(), "x".into(), "--about".into()]).is_err());
    assert_eq!(
        Recommendation::parse(&[
            "--default".into(),
            "x".into(),
            "--about".into(),
            " A  B ".into()
        ])
        .unwrap()
        .about
        .as_deref(),
        Some("a b")
    );
}

pub(crate) fn notified(cfg: &Config, ids: &[&str]) {
    for id in ids {
        let job = Job::new(cfg, id);
        let _held = job.lock().unwrap();
        crate::question::notify(cfg, &job).unwrap();
    }
}

#[test]
fn a_join_after_a_one_way_leader_timed_out_keeps_its_policy_and_deadline() {
    let (_dir, cfg) = fixture();
    asks(&cfg, "j1", None, true);
    let j1 = Job::new(&cfg, "j1");
    let due = crate::now() - 1;
    let mut q = load_raw(&j1).unwrap();
    q.due = due;
    fs::write(
        host_path(&j1, "question"),
        serde_json::to_string(&q).unwrap(),
    )
    .unwrap();
    merge(&cfg, "profile", &["j1".into()]).unwrap();
    // A stale raw deadline cannot postpone the shared question's expiry.
    q.due = crate::now() + 3600;
    fs::write(
        host_path(&j1, "question"),
        serde_json::to_string(&q).unwrap(),
    )
    .unwrap();
    assert_eq!(load(&j1).unwrap().due, due);
    {
        let _held = j1.lock().unwrap();
        expire(&cfg, &j1).unwrap();
    }
    assert!(host_path(&j1, "question-needs-owner").exists());
    assert!(load(&j1).is_none());
    assert_eq!(j1.turns().as_deref(), Some("2"));
    asks(&cfg, "j2", Some("profile"), false);
    let q2 = load(&Job::new(&cfg, "j2")).unwrap();
    assert!(q2.recommendation.one_way);
    assert_eq!(q2.due, due);
}

#[test]
fn a_shared_answer_reaches_each_waiting_job_once_and_skips_changed_questions() {
    let (_dir, cfg) = fixture();
    for id in ["j1", "j2", "j3", "j4"] {
        asks(&cfg, id, Some("profile"), false);
    }
    // j4 asks something new after the cohort is fixed: it must not get the answer.
    let cohort = cohort(&Job::new(&cfg, "j1"));
    assert_eq!(cohort.len(), 3);
    fs::write(host_path(&Job::new(&cfg, "j4"), "question-id"), "another").unwrap();
    crate::cmd::reply_answer(&cfg, "j1", "go ahead", false).unwrap();
    for id in ["j1", "j2", "j3"] {
        assert_eq!(Job::new(&cfg, id).turns().as_deref(), Some("2"), "{id}");
    }
    // j4 was open when the answer was given, so it was resumed too.
    assert_eq!(Job::new(&cfg, "j4").turns().as_deref(), Some("2"));
    // A cohort entry whose question changed or is gone is skipped.
    let (_dir, cfg) = fixture();
    for id in ["j1", "j2"] {
        asks(&cfg, id, Some("profile"), false);
    }
    assert!(!still_waiting(&Job::new(&cfg, "j2"), "profile", "stale"));
    assert!(still_waiting(
        &Job::new(&cfg, "j2"),
        "profile",
        &about::question_id(&Job::new(&cfg, "j2"))
    ));
}

#[test]
fn redeclaring_an_open_keyed_question_keeps_the_notice_where_it_is() {
    let (dir, cfg) = fixture();
    asks(&cfg, "j1", Some("profile"), false);
    asks(&cfg, "j2", Some("profile"), false);
    // j1 says the same thing again after a machinery update.
    let job = Job::new(&cfg, "j1");
    record(
        &job,
        &Recommendation {
            default: "leave it".into(),
            one_way: false,
            about: Some("profile".into()),
        },
    )
    .unwrap();
    finish(&cfg, &job, "ask").unwrap();
    assert!(follows(&Job::new(&cfg, "j2")).is_some());
    assert!(follows(&job).is_none());
    notified(&cfg, &["j1", "j2"]);
    assert!(!calls(&dir).contains("close|proj-question-j1"));
}

#[test]
fn merging_into_an_occupied_subject_keeps_its_members() {
    let (_dir, cfg) = fixture();
    asks(&cfg, "j1", Some("profile"), false);
    asks(&cfg, "j2", Some("profile"), false);
    asks(&cfg, "j3", None, false);
    merge(&cfg, "profile", &["j3".to_owned()]).unwrap();
    let lead = Job::new(&cfg, "j3");
    let mut waiting: Vec<String> = cohort(&lead).into_iter().map(|c| c.0).collect();
    waiting.sort();
    assert_eq!(waiting, ["j1", "j2"]);
    crate::cmd::reply_answer(&cfg, "j3", "yes", false).unwrap();
    for id in ["j1", "j2", "j3"] {
        assert_eq!(Job::new(&cfg, id).turns().as_deref(), Some("2"), "{id}");
    }
}

#[test]
fn a_hidden_notice_comes_back_when_its_job_takes_the_lead() {
    let (dir, cfg) = fixture();
    for id in ["j1", "j2"] {
        asks(&cfg, id, None, false);
    }
    notified(&cfg, &["j1", "j2"]);
    assert_eq!(calls(&dir).matches("ensure|").count(), 2);
    merge(&cfg, "profile", &["j1".to_owned(), "j2".to_owned()]).unwrap();
    notified(&cfg, &["j1", "j2"]);
    let hidden = if follows(&Job::new(&cfg, "j1")).is_some() {
        "j1"
    } else {
        "j2"
    };
    let lead = if hidden == "j1" { "j2" } else { "j1" };
    assert!(calls(&dir).contains(&format!("close|proj-question-{hidden}-1")));
    crate::cmd::stop(&cfg, lead, false).unwrap();
    let before = calls(&dir).matches("ensure|").count();
    notified(&cfg, &[hidden]);
    assert!(
        calls(&dir).matches("ensure|").count() > before,
        "{}",
        calls(&dir)
    );
}

#[path = "migration.rs"]
mod migration;

#[path = "notices.rs"]
mod notices;
