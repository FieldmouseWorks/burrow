use super::sorting_tests::{Box, issue};
use crate::read;
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

#[test]
fn sorting_question_retains_deadline_scratch_and_uses_the_resumed_verdict() {
    let b = Box::new();
    let d = super::sorting_tests::sorting(&b, "Which release?");
    let job = crate::job::Job::new(&b.cfg, "s1");
    fs::write(job.turn("1", "end"), "ask\n").unwrap();
    fs::write(job.path("owned"), "").unwrap();
    fs::write(
        crate::ask::host_path(&job, "question"),
        json!({"recommendation":{"default":"ready", "one_way":false},"due":crate::now()+3600})
            .to_string(),
    )
    .unwrap();
    let t = b.cfg.inbox.as_ref().unwrap().triage.clone().unwrap();
    b.pass()
        .verdicts("sam/box", &t, &[issue(7, "sam", &[])])
        .unwrap();
    assert!(d.exists(), "a question is not a verdict");
    assert!(b.posted().contains("Without an answer by"));
    assert!(b.posted().contains("reversible default"));
    assert!(crate::ask::load(&job).is_some());
    assert!(!crate::scratch::over(&job), "owned questions keep scratch");
    crate::question::Resume::prepare(&job, true)
        .unwrap()
        .commit()
        .unwrap();
    job.set_turns(2).unwrap();
    fs::write(job.turn("2", "end"), "done\n").unwrap();
    fs::write(job.turn("2", "outcome"), "done\n").unwrap();
    fs::write(
        job.turn("2", "jsonl"),
        json!({"type":"result","result":"ready\nThe owner answered."}).to_string(),
    )
    .unwrap();
    b.pass()
        .verdicts("sam/box", &t, &[issue(7, "sam", &[])])
        .unwrap();
    assert!(!d.exists());
    assert!(b.log().contains("labels[]=ready"), "{}", b.log());
    assert!(!b.log().contains("labels[]=later"));
}

#[test]
fn sorting_one_way_timeout_keeps_a_human_reply_route_without_holding_a_slot() {
    let mut b = Box::new();
    b.cfg.tools.systemd_run = "true".into();
    b.cfg.needs_owner_label = "needs-owner".into();
    burrow_test::executable(
        &b.cfg.tools.systemctl,
        "#!/bin/sh\ncase \"$*\" in *is-active*) exit 3 ;; *LoadState*) echo not-found ;; *ActiveState*) echo inactive ;; esac\n",
    );
    let d = super::sorting_tests::sorting(&b, "Which release?");
    let job = crate::job::Job::new(&b.cfg, "s1");
    crate::ask::initialize(&job, Some("https://github.com/sam/box/issues/7")).unwrap();
    fs::write(job.path("session"), "s1").unwrap();
    fs::write(job.turn("1", "end"), "ask\n").unwrap();
    fs::write(
        crate::ask::host_path(&job, "question-since"),
        (crate::now() - 2).to_string(),
    )
    .unwrap();
    fs::write(
        crate::ask::host_path(&job, "question"),
        json!({"recommendation":{"default":"keep it", "one_way":true},"due":crate::now()-1})
            .to_string(),
    )
    .unwrap();
    let t = b.cfg.inbox.as_ref().unwrap().triage.clone().unwrap();
    crate::ask::expire(&b.cfg, &job).unwrap();
    let pending = crate::ask::host_path(&job, "question-needs-owner");
    b.pass()
        .verdicts("sam/box", &t, &[issue(7, "sam", &[])])
        .unwrap();
    assert!(pending.exists());
    assert!(d.join("job").exists(), "retain the human reply route");
    assert!(!b.posted().contains("Sorted:"));
    assert!(b.posted().contains("No answer by"));
    // The same predicate used to budget sorting slots excludes this claim.
    assert!(!b.pass().sorting_slot(&d));
    for (login, kind, accepted) in [
        ("stranger", "User", false),
        ("sam", "Bot", false),
        ("sam", "User", true),
    ] {
        fs::write(b.path("comments"), json!([{"id":19,"user":{"login":login,"id":burrow_github::fake::author(login).id,"__typename":kind,"type":kind},"body":"Use the new release.","created_at":burrow_probe::answer::utc(crate::now())}]).to_string()).unwrap();
        b.pass()
            .verdicts("sam/box", &t, &[issue(7, "sam", &[])])
            .unwrap();
        assert_eq!(!pending.exists(), accepted, "{login} {kind}: {}", b.log());
    }
    assert_eq!(job.turns().as_deref(), Some("3"));
    assert!(b.pass().sorting_slot(&d));
    fs::write(job.turn("3", "end"), "done\n").unwrap();
    fs::write(job.turn("3", "outcome"), "done\n").unwrap();
    fs::write(
        job.turn("3", "jsonl"),
        json!({"type":"result","result":"ready\nThe owner answered."}).to_string(),
    )
    .unwrap();
    b.pass()
        .verdicts("sam/box", &t, &[issue(7, "sam", &["needs-owner"])])
        .unwrap();
    assert!(!d.exists());
    assert!(b.log().contains("labels[]=ready"), "{}", b.log());
}

#[test]
fn successive_sorting_questions_each_get_a_notice_and_timeout_transition_once() {
    for resolution in ["human", "reversible-timeout", "explicit", "one-way-timeout"] {
        let mut b = Box::new();
        b.cfg.tools.systemd_run = "true".into();
        burrow_test::executable(
            &b.cfg.tools.systemctl,
            "#!/bin/sh\ncase \"$*\" in *is-active*) exit 3 ;; *LoadState*) echo not-found ;; *ActiveState*) echo inactive ;; esac\n",
        );
        super::sorting_tests::sorting(&b, "First question?");
        let job = crate::job::Job::new(&b.cfg, "s1");
        fs::write(job.path("session"), "s1").unwrap();
        fs::remove_file(crate::ask::host_path(&job, "question-text")).unwrap();
        fs::write(job.turn("1", "result"), "First question?").unwrap();
        let recommendation = crate::ask::Recommendation {
            default: "keep it".into(),
            one_way: resolution == "one-way-timeout",
            about: None,
        };
        crate::ask::record(&job, &recommendation).unwrap();
        crate::ask::finish(&b.cfg, &job, "ask").unwrap();
        fs::write(job.turn("1", "end"), "ask\n").unwrap();
        let t = b.cfg.inbox.as_ref().unwrap().triage.clone().unwrap();
        let check = || {
            b.pass()
                .verdicts("sam/box", &t, &[issue(7, "sam", &[])])
                .unwrap();
        };
        check();
        check();
        assert_eq!(b.posted().matches("First question?").count(), 1);
        crate::question::Resume::prepare(&job, false)
            .unwrap()
            .commit()
            .unwrap();
        job.set_turns(2).unwrap();
        fs::write(job.turn("2", "outcome"), "done\n").unwrap();
        fs::write(job.turn("2", "result"), "Unrelated update handled.").unwrap();
        crate::ask::record(&job, &recommendation).unwrap();
        crate::ask::finish(&b.cfg, &job, "ask").unwrap();
        check();
        assert_eq!(b.posted().matches("First question?").count(), 1);
        match resolution {
            "human" => crate::question::Resume::prepare(&job, true)
                .unwrap()
                .commit()
                .unwrap(),
            "explicit" => crate::ask::finish(&b.cfg, &job, "done").unwrap(),
            _ => {
                let mut q = crate::ask::load(&job).unwrap();
                q.due = crate::now() - 1;
                fs::write(
                    crate::ask::host_path(&job, "question"),
                    serde_json::to_string(&q).unwrap(),
                )
                .unwrap();
                crate::ask::expire(&b.cfg, &job).unwrap();
                if recommendation.one_way {
                    check();
                    check();
                    assert_eq!(b.posted().matches("No answer by").count(), 1);
                    crate::question::Resume::prepare(&job, true)
                        .unwrap()
                        .commit()
                        .unwrap();
                }
            }
        }
        let turn = job.turns().unwrap().parse::<u64>().unwrap() + 1;
        job.set_turns(turn).unwrap();
        fs::write(job.turn(&turn.to_string(), "outcome"), "done\n").unwrap();
        fs::write(job.turn(&turn.to_string(), "result"), "Second question?").unwrap();
        crate::ask::record(&job, &recommendation).unwrap();
        crate::ask::finish(&b.cfg, &job, "ask").unwrap();
        check();
        check();
        assert_eq!(
            b.posted().matches("Second question?").count(),
            1,
            "{resolution}: {}",
            b.posted()
        );
        assert_eq!(
            b.posted().matches("Without an answer by").count(),
            2,
            "{resolution}"
        );
    }
}

#[test]
fn sorting_reply_is_consumed_once_despite_accepted_reconciliation_errors() {
    for fault in ["receipt", "cleanup"] {
        let mut b = Box::new();
        burrow_test::executable(
            &b.cfg.tools.systemctl,
            "#!/bin/sh\ncase \"$*\" in *is-active*) exit 3 ;; *LoadState*) echo not-found ;; *ActiveState*) echo inactive ;; esac\n",
        );
        let d = super::sorting_tests::sorting(&b, "Which release?");
        let job = crate::job::Job::new(&b.cfg, "s1");
        fs::write(job.path("session"), "s1").unwrap();
        fs::write(job.turn("1", "end"), "ask\n").unwrap();
        fs::write(
            crate::ask::host_path(&job, "question-since"),
            (crate::now() - 2).to_string(),
        )
        .unwrap();
        fs::write(
            crate::ask::host_path(&job, "question"),
            json!({
                "recommendation":{"default":"keep it", "one_way":false}, "due":crate::now()+3600,
            })
            .to_string(),
        )
        .unwrap();
        let blocked = if fault == "receipt" {
            crate::ask::host_path(&job, "resume-2.new")
        } else {
            job.path("question-answered-through")
        };
        let runner = b.path("runner");
        burrow_test::executable(
            &runner,
            format!("#!/bin/sh\nmkdir -p '{}'\nexit 0\n", blocked.display()),
        );
        b.cfg.tools.systemd_run = runner;
        let t = b.cfg.inbox.as_ref().unwrap().triage.clone().unwrap();
        fs::write(b.path("comments"), json!([{"id":19,"user":burrow_github::fake::actor("sam"),"body":"Use the new release.","created_at":burrow_probe::answer::utc(crate::now())}]).to_string()).unwrap();
        b.pass()
            .verdicts("sam/box", &t, &[issue(7, "sam", &[])])
            .unwrap();
        assert_eq!(read(&d.join("seen")).as_deref(), Some("19"));
        let job = crate::job::Job::new(&b.cfg, "s1");
        assert_eq!(job.turns().as_deref(), Some("2"));
        fs::remove_dir(blocked).unwrap();
        crate::question::accept(&job, "2").unwrap();
        b.pass()
            .verdicts("sam/box", &t, &[issue(7, "sam", &[])])
            .unwrap();
        assert_eq!(job.turns().as_deref(), Some("2"));
        assert_eq!(read(&d.join("seen")).as_deref(), Some("19"));
    }
}
