use super::super::sorting_tests::issue;
use super::tests::parent_fixture;
use super::*;
use crate::read;

#[test]
fn every_plan_scan_uses_the_complete_repository_identity() {
    for foreign in ["sam_box-extra-7", "sam_box-extra-more-7", "sam_box-foo-7"] {
        let (b, parent, _) = parent_fixture();
        let local = b.pass().plan_dir("sam/box", "7");
        fs::rename(&local, local.with_file_name(foreign)).unwrap();
        let mut pass = b.pass();
        assert!(pass.child_admissible("sam/box", "8"), "{foreign}");
        assert!(pass.child_can_close("sam/box", "8"), "{foreign}");
        pass.sequence("sam/box", &[parent]);
        assert!(b.log().is_empty(), "{}", b.log());
    }
    // A hyphen in the actual identity is valid, too.
    let (b, _, _) = parent_fixture();
    let local = b.pass().plan_dir("sam/box", "7");
    fs::rename(&local, local.with_file_name("sam_box-extra-7")).unwrap();
    assert_eq!(b.pass().plan_dirs("sam/box-extra").len(), 1);
    assert!(b.pass().plan_dirs("sam/box").is_empty());
}

#[test]
fn completed_or_unenrolled_splits_retire_without_losing_receipts() {
    for ending in ["complete", "closed", "unenrolled"] {
        let (mut b, mut parent, list) = parent_fixture();
        b.cfg.scratch = b.path("scratch");
        b.job_ending("j1", "done", Some("split"), "My split answer");
        let job = crate::job::Job::new(&b.cfg, "j1");
        let record = b.record("j1");
        fs::write(record.join("drained"), "").unwrap();
        fs::create_dir_all(crate::scratch::dir(&b.cfg, "j1")).unwrap();
        fs::write(crate::ask::host_path(&job, "question"), "{}").unwrap();
        fs::write(
            job.turn("1", "split"),
            serde_json::to_string(&list.tasks).unwrap(),
        )
        .unwrap();
        parent["labels"] = json!([{"name":"claude"}]);
        fs::write(b.path("repos_sam_box_issues_7"), parent.to_string()).unwrap();
        let receipts = fs::read(b.pass().plan_dir("sam/box", "7").join("tasks.json")).unwrap();
        if ending == "complete" {
            b.pass().jobs("sam/box", "box", &[parent]).unwrap();
        } else {
            if ending == "closed" {
                parent["state"] = json!("closed");
            } else {
                parent["labels"] = json!([]);
            }
            fs::write(b.path("repos_sam_box_issues_7"), parent.to_string()).unwrap();
            b.pass().jobs("sam/box", "box", &[]).unwrap();
            assert_eq!(
                fs::read(b.pass().plan_dir("sam/box", "7").join("tasks.json")).unwrap(),
                receipts
            );
        }
        assert!(!record.exists(), "{ending}");
        assert!(job.turn("1", "stopped").exists(), "{ending}");
        assert!(!crate::scratch::dir(&b.cfg, "j1").exists(), "{ending}");
        assert!(
            !crate::ask::host_path(&job, "question").exists(),
            "{ending}"
        );
        assert!(b.posted().contains("My split answer"), "{ending}");
        b.pass().jobs("sam/box", "box", &[]).unwrap();
        assert_eq!(b.posted().matches("My split answer").count(), 1);
    }
}

#[test]
fn stale_split_proposals_resume_the_updated_verified_task_and_keep_receipts() {
    for change in [
        "body",
        "title",
        "comment-add",
        "comment-edit",
        "comment-delete",
    ] {
        let (mut b, mut parent, mut list) = parent_fixture();
        list.children[0].merged_pr = Some("https://github.com/sam/box/pull/10".into());
        list.children[1].number = None;
        list.children[1].attempted = true;
        assert!(b.pass().save_list("sam/box", "7", &list));
        b.cfg.tools.systemd_run = "true".into();
        burrow_test::executable(
            &b.cfg.tools.systemctl,
            "#!/bin/sh\ncase \"$*\" in *is-active*) exit 3 ;; *LoadState*) echo not-found ;; esac\n",
        );
        b.job_ending("j1", "done", Some("split"), "old proposal");
        let record = b.record("j1");
        let job = crate::job::Job::new(&b.cfg, "j1");
        fs::write(job.path("session"), "s").unwrap();
        parent["labels"] = json!([{"name":"claude"}]);
        let comment = |body: &str| json!({"id":5,"user":{"login":"sam"},"body":body,"created_at":"2020-01-01T00:00:00Z"});
        let old_comments = if matches!(change, "comment-edit" | "comment-delete") {
            vec![comment("old")]
        } else {
            vec![]
        };
        Pass::hand(
            &record,
            &b.pass().issue_text("sam/box", &parent, &old_comments),
        )
        .unwrap();
        fs::write(
            record.join("seen"),
            if old_comments.is_empty() { "0" } else { "5" },
        )
        .unwrap();
        match change {
            "body" => parent["body"] = json!("new body"),
            "title" => parent["title"] = json!("new title"),
            _ => (),
        }
        let comments = match change {
            "comment-add" | "comment-edit" => vec![comment("updated")],
            _ => vec![],
        };
        fs::write(b.path("comments"), json!([comments]).to_string()).unwrap();
        fs::write(b.path("repos_sam_box_issues_7"), parent.to_string()).unwrap();
        crate::planning::declare_split(&job, "1", &list.tasks).unwrap();
        let receipts = fs::read(b.pass().plan_dir("sam/box", "7").join("tasks.json")).unwrap();
        b.pass()
            .jobs("sam/box", "box", std::slice::from_ref(&parent))
            .unwrap();
        assert!(job.turn("1", "split-invalid").exists(), "{change}");
        assert!(crate::planning::admit(&job).is_ok(), "{change}");
        assert_eq!(job.turns().as_deref(), Some("1"));
        b.pass()
            .jobs("sam/box", "box", std::slice::from_ref(&parent))
            .unwrap();
        let whole = b.pass().issue_text("sam/box", &parent, &comments);
        assert!(
            fs::read_to_string(job.path("prompt.2"))
                .unwrap()
                .contains(&whole),
            "{change}"
        );
        assert!(Pass::handed(&record, &whole), "{change}");
        assert_eq!(
            fs::read(b.pass().plan_dir("sam/box", "7").join("tasks.json")).unwrap(),
            receipts
        );
        if !comments.is_empty() {
            assert_eq!(read(&record.join("seen")).as_deref(), Some("5"));
        }
    }
}

#[test]
fn worker_admission_uses_the_verified_payload_it_launches_without_a_second_read() {
    for child in [false, true] {
        let (mut b, mut task, _) = parent_fixture();
        b.cfg.projects = b.path("projects");
        fs::create_dir_all(b.cfg.projects.join("box")).unwrap();
        b.cfg.tools.systemd_run = "true".into();
        b.cfg.tools.systemctl = "false".into();
        if child {
            task = issue(8, "sam", &["claude"]);
        } else {
            fs::remove_file(b.pass().plan_dir("sam/box", "7").join("tasks.json")).unwrap();
        }
        let n = raw(&task["number"]);
        let mut approved = task.clone();
        approved["body"] = json!("approved A");
        b.pass()
            .keep_planned(
                "sam/box",
                &n,
                &b.pass().issue_text("sam/box", &approved, &[]),
            )
            .unwrap();
        task["body"] = json!("unapproved B");
        fs::write(
            b.path(&format!("repos_sam_box_issues_{n}")),
            task.to_string(),
        )
        .unwrap();
        b.pass().new_issues("sam/box", &[task]).unwrap();
        assert!(
            !b.cfg
                .inbox
                .as_ref()
                .unwrap()
                .state
                .join(format!("sam_box-{n}/job"))
                .exists()
        );
        // Only one source-history query is needed to make this decision.
        assert_eq!(
            fs::read_to_string(b.path("api-log"))
                .unwrap()
                .matches("graphql")
                .count(),
            1
        );
    }
}

#[test]
fn launched_workers_share_approved_prompt_handed_text_and_comment_cursor() {
    for kind in ["ordinary", "generated", "reused"] {
        let (mut b, mut task, mut list) = parent_fixture();
        b.cfg.projects = b.path("projects");
        fs::create_dir_all(b.cfg.projects.join("box")).unwrap();
        b.cfg.tools.systemd_run = "true".into();
        b.cfg.tools.systemctl = "false".into();
        if kind == "ordinary" {
            fs::remove_file(b.pass().plan_dir("sam/box", "7").join("tasks.json")).unwrap();
        } else {
            for (i, planned) in list.tasks.iter_mut().enumerate() {
                let mut child = issue(8 + i as u64, "sam", &["claude"]);
                child["title"] = json!(planned.title);
                if kind == "reused" {
                    planned.issue = Some(8 + i as u64);
                }
                child["body"] = json!(if kind == "reused" {
                    planned.body.clone()
                } else {
                    Pass::child_body("sam/box", "7", i, planned)
                });
                fs::write(
                    b.path(&format!("repos_sam_box_issues_{}", 8 + i)),
                    child.to_string(),
                )
                .unwrap();
                if i == 0 {
                    task = child;
                }
            }
            assert!(b.pass().save_list("sam/box", "7", &list));
            assert!(b.pass().split_children("sam/box", "7", &list.tasks));
        }
        let n = raw(&task["number"]);
        let comments = vec![json!({"id":42,"user":{"login":"sam"},"body":"approved comment \t\n"})];
        fs::write(b.path("comments"), json!([comments]).to_string()).unwrap();
        if kind != "ordinary" {
            // The fixture shares one comments response across issue numbers.
            let parent: Value =
                serde_json::from_slice(&fs::read(b.path("repos_sam_box_issues_7")).unwrap())
                    .unwrap();
            let parent_input = b.pass().issue_text("sam/box", &parent, &comments);
            b.pass()
                .keep_planned("sam/box", "7", &parent_input)
                .unwrap();
        }
        fs::write(
            b.path(&format!("repos_sam_box_issues_{n}")),
            task.to_string(),
        )
        .unwrap();
        let snapshot = b.pass().issue_text("sam/box", &task, &comments);
        b.pass().keep_planned("sam/box", &n, &snapshot).unwrap();
        b.pass().new_issues("sam/box", &[task]).unwrap();
        let record = b
            .cfg
            .inbox
            .as_ref()
            .unwrap()
            .state
            .join(format!("sam_box-{n}"));
        let id = read(&record.join("job")).unwrap();
        assert!(
            fs::read_to_string(b.cfg.jobs.join(id).join("prompt.1"))
                .unwrap()
                .contains(&snapshot),
            "{kind}"
        );
        assert_eq!(
            fs::read_to_string(record.join("input")).unwrap(),
            snapshot,
            "{kind}"
        );
        assert_eq!(read(&record.join("seen")).as_deref(), Some("42"), "{kind}");
    }
}
