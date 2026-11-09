use super::sorting_tests::{Box, issue};
use serde_json::json;
use std::fs;

#[test]
fn unattended_question_is_labelled_and_one_way_timeout_keeps_issue_open_and_releases_drain() {
    let b = Box::new();
    burrow_test::executable(
        &b.cfg.tools.systemctl,
        "#!/bin/sh\ncase \"$*\" in *is-active*) exit 3 ;; *LoadState*) echo not-found ;; *ActiveState*) echo inactive ;; esac\n",
    );
    b.job_ending("j1", "done", Some("ask"), "Which release?");
    fs::write(b.cfg.jobs.join("j1/1.result"), "Which release?").unwrap();
    let d = b.record("j1");
    fs::write(d.join("drained"), "sam/box#7\n").unwrap();
    let job = crate::job::Job::new(&b.cfg, "j1");
    fs::write(
        crate::ask::host_path(&job, "question"),
        json!({"recommendation":{"default":"keep it", "one_way":true},"due":crate::now()+3600})
            .to_string(),
    )
    .unwrap();
    let now = issue(7, "sam", &["claude"]);
    let mut initial = b.pass();
    initial
        .jobs("sam/box", "box", std::slice::from_ref(&now))
        .unwrap();
    assert!(
        initial.wake.is_empty(),
        "question deadlines belong to proj-watch alone: {:?}",
        initial.wake
    );
    assert!(b.log().contains(&b.cfg.waiting_owner_label), "{}", b.log());
    assert!(crate::ask::host_path(&job, "label").exists());
    assert!(!b.log().contains("state=closed"));
    // The timeout's completed host turn is posted through the same writer.
    fs::write(
        crate::ask::host_path(&job, "question"),
        json!({"recommendation":{"default":"keep it", "one_way":true},"due":crate::now()-1})
            .to_string(),
    )
    .unwrap();
    crate::ask::expire(&b.cfg, &job).unwrap();
    b.pass()
        .jobs("sam/box", "box", std::slice::from_ref(&now))
        .unwrap();
    assert!(b.log().contains(&b.cfg.needs_owner_label), "{}", b.log());
    assert!(!b.log().contains("state=closed"), "{}", b.log());
    assert!(b.posted().contains("No answer by"));
    assert!(
        crate::ask::owner_lines(&b.cfg)
            .iter()
            .any(|(_, _, text)| text.contains("Which release?"))
    );
    assert!(!d.join("drained").exists());
    assert!(d.join("job").exists());
}

#[test]
fn steward_and_sorting_questions_use_the_same_visibility_path_without_inbox_records() {
    let b = Box::new();
    b.job_ending("sort1", "done", Some("ask"), "Which release?");
    let job = crate::job::Job::new(&b.cfg, "sort1");
    crate::ask::initialize(&job, Some("https://github.com/sam/box/issues/7")).unwrap();
    fs::write(job.path("link"), "https://github.com/sam/box/issues/7\n").unwrap();
    fs::write(
        job.path("link"),
        "https://github.com/sam/unrelated/issues/99",
    )
    .unwrap();
    fs::write(job.turn("1", "result"), "Which release?").unwrap();
    fs::write(
        crate::ask::host_path(&job, "question"),
        json!({"recommendation":{"default":"keep it", "one_way":false},"due":crate::now()+3600})
            .to_string(),
    )
    .unwrap();
    fs::write(b.path("fail"), "labels\n").unwrap();
    assert!(crate::ask::visibility(&b.cfg, &job).is_err());
    assert!(!crate::ask::host_path(&job, "label").exists());
    fs::write(b.path("fail"), "").unwrap();
    crate::ask::visibility(&b.cfg, &job).unwrap();
    assert!(crate::ask::host_path(&job, "label").exists());
    let before = b.log();
    crate::ask::visibility(&b.cfg, &job).unwrap();
    assert_eq!(
        b.log(),
        before,
        "an unchanged question makes no duplicate request"
    );
    let rows = crate::ask::owner_lines(&b.cfg);
    assert!(
        rows.iter()
            .any(|(_, link, text)| link.contains("issues/7") && text.contains("Which release?"))
    );
    fs::write(
        b.path("repos_sam_box_issues_7"),
        json!({"labels":[{"name":b.cfg.waiting_owner_label}]}).to_string(),
    )
    .unwrap();
    crate::ask::clear(&job);
    crate::ask::visibility(&b.cfg, &job).unwrap();
    assert!(!crate::ask::host_path(&job, "label").exists());
    assert!(b.log().contains("DELETE"), "{}", b.log());
    assert!(crate::ask::owner_lines(&b.cfg).is_empty());
}

#[test]
fn writable_job_metadata_cannot_redirect_host_label_authority() {
    let b = Box::new();
    b.job_ending("j1", "done", Some("ask"), "Which release?");
    b.record("j1");
    let job = crate::job::Job::new(&b.cfg, "j1");
    fs::write(
        crate::ask::host_path(&job, "question"),
        json!({"recommendation":{"default":"keep it", "one_way":false},"due":crate::now()+3600})
            .to_string(),
    )
    .unwrap();
    fs::write(
        job.path("link"),
        "https://github.com/sam/unrelated/issues/99",
    )
    .unwrap();
    fs::write(
        job.path("question-label"),
        json!({"repo":"sam/unrelated","number":"99","name":"hold"}).to_string(),
    )
    .unwrap();
    fs::write(
        b.path("repos_sam_unrelated_issues_99"),
        json!({"labels":[{"name":"hold"}]}).to_string(),
    )
    .unwrap();
    crate::ask::visibility(&b.cfg, &job).unwrap();
    assert!(!b.log().contains("sam/unrelated"), "{}", b.log());
    assert!(
        b.log().contains("repos/sam/box/issues/7/labels"),
        "{}",
        b.log()
    );
}
