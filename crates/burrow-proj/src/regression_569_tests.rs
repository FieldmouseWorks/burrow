//! Exercise the watcher path, including jobs with no GitHub target.
use crate::{config::Config, job::Job};
use std::fs;
fn fixture() -> (tempfile::TempDir, Config) {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
    cfg.jobs = dir.path().join("jobs");
    cfg.scratch = dir.path().join("scratch");
    cfg.wake = dir.path().join("wake.json");
    cfg.kick.clear();
    cfg.notify = None;
    cfg.tools.systemctl = dir.path().join("systemctl");
    burrow_test::executable(
        &cfg.tools.systemctl,
        "#!/bin/sh\ncase \"$*\" in *is-active*) exit 3 ;; *LoadState*) echo not-found ;; *ActiveState*) echo inactive ;; esac\n",
    );
    cfg.tools.systemd_run = "true".into();
    (dir, cfg)
}
fn asking(cfg: &Config, one_way: bool, due: i64) -> Job<'_> {
    let job = Job::new(cfg, "j1");
    fs::create_dir_all(&job.dir).unwrap();
    crate::policy::bind(
        &job,
        crate::policy::Identity::Local,
        crate::policy::Kind::Manual,
    )
    .unwrap();
    job.set_turns(1).unwrap();
    fs::create_dir_all(cfg.jobs.join(".question-control").join(job.id)).unwrap();
    fs::write(job.path("session"), "s1\n").unwrap();
    fs::write(job.turn("1", "end"), "ask\n").unwrap();
    fs::write(job.turn("1", "outcome"), "done\n").unwrap();
    fs::write(cfg.jobs.join(".question-control").join(job.id).join("question"), serde_json::json!({"recommendation":{"default":"keep the change","one_way":one_way},"due":due}).to_string()).unwrap();
    job
}
fn expired(one_way: bool) {
    let (_dir, cfg) = fixture();
    let job = asking(&cfg, one_way, crate::now() - 1);
    crate::watch::look(&cfg, "").unwrap();
    assert_eq!(
        job.turns().as_deref(),
        Some("2"),
        "one_way={one_way}: an ask cannot wait forever"
    );
    if one_way {
        assert_eq!(crate::read(&job.turn("2", "end")).as_deref(), Some("done"));
        assert!(
            cfg.jobs
                .join(".question-control")
                .join(job.id)
                .join("question-needs-owner")
                .exists()
        );
    } else {
        let prompt = crate::read(&job.path("prompt.2")).unwrap();
        assert!(prompt.contains("No answer by"));
        assert!(prompt.contains("keep the change"));
    }
}
#[test]
fn watcher_resumes_an_expired_reversible_question_without_a_github_wait() {
    expired(false);
}
#[test]
fn watcher_completes_an_expired_one_way_question_without_a_github_wait() {
    expired(true);
}
#[test]
fn watcher_books_the_question_deadline_without_a_github_wait() {
    let (_dir, cfg) = fixture();
    let due = crate::now() + 3600;
    let _job = asking(&cfg, false, due);
    crate::watch::look(&cfg, "").unwrap();
    let wake: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&cfg.wake).unwrap()).unwrap();
    assert_eq!(wake[""].as_i64(), Some(due));
}
