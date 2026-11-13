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
    for ending in ["complete", "retry-finalization", "closed", "unenrolled"] {
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
        if ending == "complete" || ending == "retry-finalization" {
            if ending == "retry-finalization" {
                fs::write(b.path("fail"), "labels[]=tracker").unwrap();
                b.pass().jobs("sam/box", "box", &[parent.clone()]).unwrap();
                assert!(record.exists());
                assert!(!b.log().contains("DELETE repos/sam/box/issues/7/labels/"));
                fs::remove_file(b.path("fail")).unwrap();
            }
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
        let comment = |body: &str| json!({"id":5,"user":burrow_github::fake::actor("sam"),"body":body,"created_at":"2020-01-01T00:00:00Z"});
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
        task["labels"] = json!([{"name":"claude"}]);
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
            task["labels"] = json!([{"name":"claude"}]);
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
        let comments = vec![
            json!({"id":42,"user":burrow_github::fake::actor("sam"),"body":"approved comment \t\n"}),
        ];
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

#[test]
fn revisions_bind_source_and_preserve_exact_task_receipts_across_reordering() {
    let (b, mut parent, mut list) = parent_fixture();
    list.children[0].merged_pr = Some("landed".into());
    list.children[1].number = None;
    list.children[1].attempted = true;
    assert!(b.pass().save_list("sam/box", "7", &list));
    let old = list.tasks.clone();
    parent["body"] = json!("revised scope");
    fs::write(b.path("repos_sam_box_issues_7"), parent.to_string()).unwrap();
    let source = b.pass().issue_text("sam/box", &parent, &[]);
    let replacement = Task {
        title: "replacement".into(),
        ..old[1].clone()
    };
    let revised = vec![old[1].clone(), replacement.clone()];
    assert!(b.pass().prepare_plan("sam/box", "7", &source, &revised));
    let path = b.pass().plan_dir("sam/box", "7").join("tasks.json");
    let kept: List = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(kept.source.as_deref(), Some(source.as_str()));
    assert!(kept.children[0].attempted);
    assert_eq!(kept.children[0].number, None);
    assert!(
        kept.children[0]
            .marker
            .as_ref()
            .unwrap()
            .ends_with(" 2 -->")
    );
    assert!(!kept.children[1].attempted);
    assert!(
        kept.children[1]
            .marker
            .as_ref()
            .unwrap()
            .contains("revision-1")
    );
    assert_eq!(kept.retained[0].child.merged_pr.as_deref(), Some("landed"));
    assert!(!b.pass().child_admissible("sam/box", "8"));
    assert_eq!(b.pass().same_source("sam/box", &parent), Some(true));
    // A crash between list publication and approval publication is closed.
    b.pass()
        .keep_planned("sam/box", "7", "old approval")
        .unwrap();
    assert_eq!(b.pass().same_source("sam/box", &parent), Some(false));
    assert!(b.pass().prepare_plan("sam/box", "7", &source, &revised));
    // Reinstating the identical completed task recovers its original receipt.
    let restored = vec![replacement, old[0].clone()];
    assert!(b.pass().prepare_plan("sam/box", "7", &source, &restored));
    let kept: List = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(kept.children[1].number, Some(8));
    assert_eq!(kept.children[1].merged_pr.as_deref(), Some("landed"));
    assert!(
        kept.retained
            .iter()
            .any(|r| r.child.attempted && r.child.number.is_none())
    );
}

#[test]
fn triage_uses_the_pass_repository_for_every_state_lookup() {
    for kind in ["tracker", "transaction", "approval", "worker", "split"] {
        let (b, mut issue, _) = parent_fixture();
        issue["repository_url"] = json!("https://api.github.com/repos/Sam/Box");
        let n = "7";
        let t = b.cfg.inbox.as_ref().unwrap().triage.as_ref().unwrap();
        match kind {
            "transaction" => {
                let d = b
                    .cfg
                    .inbox
                    .as_ref()
                    .unwrap()
                    .state
                    .join(".triage/sam_box-7");
                fs::create_dir_all(&d).unwrap();
                fs::write(d.join("plan"), "[]").unwrap();
            }
            "approval" => {
                issue["labels"] = json!([{"name":"ready"}]);
                fs::remove_file(b.pass().plan_dir("sam/box", n).join("tasks.json")).unwrap();
            }
            "worker" => {
                issue["labels"] = json!([{"name":"claude"}]);
                let d = b.cfg.inbox.as_ref().unwrap().state.join("sam_box-7");
                fs::create_dir_all(&d).unwrap();
                fs::write(d.join("job"), "j1").unwrap();
            }
            "split" => {
                let record = b.cfg.inbox.as_ref().unwrap().state.join("sam_box-7");
                fs::create_dir_all(&record).unwrap();
                fs::write(record.join("job"), "j1").unwrap();
                issue["labels"] = json!([{"name":"claude"}]);
                fs::write(
                    b.pass().plan_dir("sam/box", n).join("worker-split"),
                    "pending",
                )
                .unwrap();
            }
            _ => (),
        }
        fs::write(b.path("repos_sam_box_issues_7"), issue.to_string()).unwrap();
        let source = b.pass().issue_text("sam/box", &issue, &[]);
        b.pass().keep_planned("sam/box", n, &source).unwrap();
        assert_eq!(
            b.pass().sorted("sam/box", t, &issue, &Default::default()),
            kind != "transaction",
            "{kind}"
        );
    }
}

#[test]
fn invalidated_splits_resume_even_after_edits_are_reverted_and_launch_retries() {
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
        let comment = |body: &str| json!({"id":5,"user":burrow_github::fake::actor("sam"),"body":body,"created_at":"2020-01-01T00:00:00Z"});
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
        let old_input = fs::read_to_string(record.join("input")).unwrap();
        // Restore every original source field after invalidation, before delivery.
        // Use the fixture's original issue rather than assume its rendering.
        let original = issue(7, "sam", &["claude"]);
        parent["title"] = original["title"].clone();
        parent["body"] = original["body"].clone();
        let comments = old_comments;
        assert_eq!(
            b.pass().issue_text("sam/box", &parent, &comments),
            old_input
        );
        fs::write(b.path("comments"), json!([comments]).to_string()).unwrap();
        fs::write(b.path("repos_sam_box_issues_7"), parent.to_string()).unwrap();
        // Failed launch must leave the invalidation recoverable.
        drop(job);
        b.cfg.tools.systemd_run = "false".into();
        b.pass()
            .jobs("sam/box", "box", std::slice::from_ref(&parent))
            .unwrap();
        b.cfg.tools.systemd_run = "true".into();
        b.pass()
            .jobs("sam/box", "box", std::slice::from_ref(&parent))
            .unwrap();
        let job = crate::job::Job::new(&b.cfg, "j1");
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
        let cursor = fs::read(record.join("seen")).unwrap();
        let prompt = fs::read(job.path("prompt.2")).unwrap();
        b.pass()
            .jobs("sam/box", "box", std::slice::from_ref(&parent))
            .unwrap();
        assert_eq!(job.turns().as_deref(), Some("2"));
        assert_eq!(fs::read(record.join("seen")).unwrap(), cursor);
        assert_eq!(fs::read(job.path("prompt.2")).unwrap(), prompt);
    }
}

#[test]
fn planner_and_worker_revisions_complete_without_reauthorizing_obsolete_children() {
    for owner in ["planner", "worker"] {
        let (b, mut parent, mut list) = parent_fixture();
        list.children[0].merged_pr = Some("landed".into());
        list.children[1].number = None;
        list.children[1].attempted = true;
        assert!(b.pass().save_list("sam/box", "7", &list));
        let tasks = vec![
            Task {
                title: "replacement".into(),
                ..list.tasks[1].clone()
            },
            list.tasks[0].clone(),
        ];
        parent["body"] = json!("revised source");
        parent["labels"] = json!([{"name": if owner == "worker" { "claude" } else { "tracker" }}]);
        parent["repository_url"] = json!("https://api.github.com/repos/sam/box");
        fs::write(b.path("repos_sam_box_issues_7"), parent.to_string()).unwrap();
        let source = b.pass().issue_text("sam/box", &parent, &[]);
        let mut new_child = issue(10, "sam", &["later"]);
        new_child["title"] = json!(tasks[0].title);
        new_child["body"] = json!(Pass::receipt_body(
            "sam/box",
            "7",
            0,
            &tasks[0],
            &Child {
                marker: Some("<!-- proj-child sam/box#7 revision-1 1 -->".into()),
                ..Child::default()
            }
        ));
        let mut obsolete = issue(9, "sam", &["ready"]);
        obsolete["body"] = json!(Pass::child_body("sam/box", "7", 1, &list.tasks[1]));
        fs::write(
            b.path("repos_sam_box_issues"),
            json!([new_child, obsolete]).to_string(),
        )
        .unwrap();
        fs::write(b.path("repos_sam_box_issues_10"), new_child.to_string()).unwrap();
        if owner == "worker" {
            b.job_ending("j1", "done", Some("split"), "revised proposal");
            let record = b.record("j1");
            Pass::hand(&record, &source).unwrap();
            fs::write(
                b.cfg.jobs.join("j1/1.split"),
                serde_json::to_string(&tasks).unwrap(),
            )
            .unwrap();
            b.pass().jobs("sam/box", "box", &[parent.clone()]).unwrap();
            assert!(!record.exists(), "{}", b.log());
        } else {
            b.job(
                "planner",
                "done",
                &format!("split\n{}", serde_json::to_string(&tasks).unwrap()),
            );
            let record = b
                .cfg
                .inbox
                .as_ref()
                .unwrap()
                .state
                .join(".triage/sam_box-7");
            fs::create_dir_all(&record).unwrap();
            fs::write(record.join("job"), "planner").unwrap();
            Pass::hand(&record, &source).unwrap();
            let steps = vec![
                super::super::triage::Step::Split(tasks.clone()),
                super::super::triage::Step::Label("tracker".into()),
            ];
            fs::write(record.join("plan"), serde_json::to_string(&steps).unwrap()).unwrap();
            let t = b.cfg.inbox.as_ref().unwrap().triage.as_ref().unwrap();
            b.pass().verdicts("sam/box", t, &[parent.clone()]).unwrap();
            assert!(!record.exists(), "{}", b.log());
        }
        assert_eq!(b.pass().same_source("sam/box", &parent), Some(true));
        assert!(
            b.pass().child_admissible("sam/box", "10"),
            "{owner}: {}",
            b.log()
        );
        assert!(!b.pass().child_admissible("sam/box", "9"), "{owner}");
        assert!(
            !b.log().contains("-X POST repos/sam/box/issues "),
            "{owner}"
        );
        let kept: List = serde_json::from_slice(
            &fs::read(b.pass().plan_dir("sam/box", "7").join("tasks.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(kept.children[1].merged_pr.as_deref(), Some("landed"));
        assert!(kept.retained[0].child.attempted);
    }
}

#[test]
fn replanning_as_ready_keeps_history_without_admitting_old_children_or_closing_parent() {
    let (b, mut parent, _) = parent_fixture();
    parent["body"] = json!("now one bounded task");
    fs::write(b.path("repos_sam_box_issues_7"), parent.to_string()).unwrap();
    let source = b.pass().issue_text("sam/box", &parent, &[]);
    assert!(b.pass().prepare_plan("sam/box", "7", &source, &[]));
    assert!(!b.pass().has_task_list(Some("sam/box"), "7"));
    assert!(b.pass().approved_input("sam/box", "7", &source));
    assert!(!b.pass().child_admissible("sam/box", "8"));
    b.pass().sequence("sam/box", &[parent]);
    assert!(!b.log().contains("state=closed"));
    let kept: List = serde_json::from_slice(
        &fs::read(b.pass().plan_dir("sam/box", "7").join("tasks.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(kept.retained.len(), 2);
    // Incomplete approval publication must not launch the old input either.
    b.pass().keep_planned("sam/box", "7", "old input").unwrap();
    assert!(!b.pass().approved_input("sam/box", "7", "old input"));
}

#[test]
fn edited_task_fields_never_inherit_completed_or_ambiguous_receipts() {
    for field in ["title", "body", "check", "issue"] {
        let (b, parent, mut list) = parent_fixture();
        list.children[0].merged_pr = Some("landed".into());
        list.children[1].number = None;
        list.children[1].attempted = true;
        assert!(b.pass().save_list("sam/box", "7", &list));
        let mut tasks = list.tasks.clone();
        match field {
            "title" => tasks[0].title = "edited".into(),
            "body" => tasks[0].body = "edited".into(),
            "check" => tasks[0].check = "edited".into(),
            "issue" => tasks[0].issue = Some(99),
            _ => unreachable!(),
        }
        let source = b.pass().issue_text("sam/box", &parent, &[]);
        assert!(b.pass().prepare_plan("sam/box", "7", &source, &tasks));
        let kept: List = serde_json::from_slice(
            &fs::read(b.pass().plan_dir("sam/box", "7").join("tasks.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(kept.children[0].number, None, "{field}");
        assert_eq!(kept.children[0].merged_pr, None, "{field}");
        assert!(!kept.children[0].attempted, "{field}");
        assert!(kept.children[1].attempted, "{field}");
        assert!(
            kept.children[1]
                .marker
                .as_ref()
                .unwrap()
                .ends_with(" 2 -->")
        );
        assert_eq!(kept.retained[0].child.number, Some(8));
        assert_eq!(kept.retained[0].child.merged_pr.as_deref(), Some("landed"));
    }
}

#[test]
fn active_split_parents_stay_out_of_planning_and_drain_until_ready_revision() {
    for interrupted in [false, true] {
        let (b, mut parent, list) = parent_fixture();
        parent["labels"] = json!([{"name":"ready"},{"name":"tracker"},{"name":"codex"}]);
        parent["repository_url"] = json!("https://api.github.com/repos/sam/box");
        fs::write(b.path("repos_sam_box_issues_7"), parent.to_string()).unwrap();
        for n in [8, 9] {
            let mut c = issue(n, "sam", &["later"]);
            c["state"] = json!("open");
            fs::write(b.path(&format!("repos_sam_box_issues_{n}")), c.to_string()).unwrap();
        }
        if interrupted {
            fs::write(b.path("fail"), "DELETE repos/sam/box/issues/7/labels/ready").unwrap();
        }
        assert_eq!(
            b.pass().split_children("sam/box", "7", &list.tasks),
            !interrupted
        );
        let t = b.cfg.inbox.as_ref().unwrap().triage.as_ref().unwrap();
        assert!(b.pass().sorted("sam/box", t, &parent, &Default::default()));
        assert_eq!(b.pass().pick_ready(t, &[parent.clone()]), None);
        let record = b.cfg.inbox.as_ref().unwrap().state.join("sam_box-7");
        fs::create_dir_all(&record).unwrap();
        fs::write(record.join("drained"), "sam/box#7").unwrap();
        let view = b.pass().view(t, crate::now());
        assert!(
            view.slots
                .iter()
                .all(|s| s.state == crate::snapshot::SlotState::Idle),
            "{:?}",
            view.slots
        );
        let mut child = issue(8, "sam", &["ready"]);
        child["state"] = json!("open");
        child["repository_url"] = parent["repository_url"].clone();
        fs::write(b.path("repos_sam_box_issues_8"), child.to_string()).unwrap();
        b.pass()
            .note_ready("sam/box", &[parent.clone(), child])
            .unwrap();
        b.pass().drain().unwrap();
        assert!(
            b.log().contains("issues/8/labels -f labels[]=codex"),
            "{}",
            b.log()
        );
        assert!(
            b.log()
                .contains("DELETE repos/sam/box/issues/7/labels/ready")
        );
        let source = b.pass().issue_text("sam/box", &parent, &[]);
        assert!(b.pass().prepare_plan("sam/box", "7", &source, &[]));
        fs::remove_dir_all(record).unwrap();
        parent["labels"] = json!([{"name":"ready"}]);
        assert_eq!(
            b.pass().pick_ready(t, &[parent]),
            Some(("sam/box".into(), "7".into()))
        );
    }
}

#[test]
fn unstick_children_keep_admission_execution_and_sequence() {
    use crate::engine::Engine;
    for kind in ["generated", "reused", "nested", "offered"] {
        for engine in [Engine::Claude, Engine::Codex] {
            let (mut b, mut parent, mut list) = parent_fixture();
            let dir = b.path("settings");
            crate::config::tests::routes(&mut b.cfg, &dir, &format!("stuck {}", engine.name()));
            b.cfg.projects = b.path("projects");
            fs::create_dir_all(b.cfg.projects.join("box")).unwrap();
            b.cfg.tools.systemd_run = "true".into();
            b.cfg.tools.systemctl = "false".into();
            let inbox = b.cfg.inbox.as_mut().unwrap();
            inbox.triage.as_mut().unwrap().drain = Some(engine);
            let label = inbox.unstick_label.clone();
            parent["labels"] = json!([{"name":"tracker"},{"name":label}]);
            fs::write(b.path("repos_sam_box_issues_7"), parent.to_string()).unwrap();
            if kind == "reused" {
                list.tasks[0].issue = Some(8);
            }
            if kind == "offered" {
                list.children[0].offered = true;
            }
            assert!(b.pass().save_list("sam/box", "7", &list));
            let mut child = issue(8, "sam", &["later"]);
            child["state"] = json!("open");
            fs::write(b.path("repos_sam_box_issues_8"), child.to_string()).unwrap();
            b.pass().sequence("sam/box", &[parent]);
            assert!(
                b.log()
                    .contains(&format!("issues/8/labels -f labels[]={label}")),
                "{}",
                b.log()
            );
            child["labels"] = json!([{"name":"ready"},{"name":label}]);
            let target = if kind == "nested" {
                let source = b.pass().issue_text("sam/box", &child, &[]);
                b.pass().keep_planned("sam/box", "8", &source).unwrap();
                list.children[0].number = Some(10);
                list.children[1].number = Some(11);
                assert!(b.pass().save_list("sam/box", "8", &list));
                fs::write(b.path("repos_sam_box_issues_8"), child.to_string()).unwrap();
                let mut next = issue(10, "sam", &["later"]);
                next["state"] = json!("open");
                fs::write(b.path("repos_sam_box_issues_10"), next.to_string()).unwrap();
                b.pass().sequence("sam/box", &[child]);
                assert!(
                    b.log()
                        .contains(&format!("issues/10/labels -f labels[]={label}"))
                );
                child = next;
                child["labels"] = json!([{"name":"ready"},{"name":label}]);
                10
            } else {
                8
            };
            child["repository_url"] = json!("https://api.github.com/repos/sam/box");
            fs::write(
                b.path(&format!("repos_sam_box_issues_{target}")),
                child.to_string(),
            )
            .unwrap();
            let source = b.pass().issue_text("sam/box", &child, &[]);
            b.pass()
                .keep_planned("sam/box", &target.to_string(), &source)
                .unwrap();
            let busy = b.cfg.inbox.as_ref().unwrap().state.join("sam_box-99");
            fs::create_dir_all(&busy).unwrap();
            fs::write(busy.join("drained"), "sam/box#99").unwrap();
            fs::write(busy.join("job"), "busy").unwrap();
            let (slots, count) = if engine == Engine::Codex {
                (".codex-slots", b.cfg.codex_slots)
            } else {
                (".slots", b.cfg.max_jobs)
            };
            fs::create_dir_all(b.cfg.jobs.join(slots)).unwrap();
            let _held: Vec<_> = (1..=count)
                .map(|n| {
                    let f = fs::File::create(b.cfg.jobs.join(slots).join(n.to_string())).unwrap();
                    f.lock().unwrap();
                    f
                })
                .collect();
            b.pass().note_ready("sam/box", &[child.clone()]).unwrap();
            b.pass().drain().unwrap();
            assert!(
                b.log().contains(&format!(
                    "issues/{target}/labels -f labels[]={}",
                    engine.name()
                )),
                "{kind}/{engine:?}: {}",
                b.log()
            );
            child["labels"]
                .as_array_mut()
                .unwrap()
                .push(json!({"name":engine.name()}));
            fs::write(
                b.path(&format!("repos_sam_box_issues_{target}")),
                child.to_string(),
            )
            .unwrap();
            b.pass().new_issues("sam/box", &[child]).unwrap();
            let record = b
                .cfg
                .inbox
                .as_ref()
                .unwrap()
                .state
                .join(format!("sam_box-{target}"));
            let id = read(&record.join("job")).unwrap();
            let job = crate::job::Job::new(&b.cfg, &id);
            assert!(job.path("unstick").exists());
            assert_eq!(crate::engine::choice(&b.cfg, &job).unwrap().engine, engine);
            assert!(!b.pass().child_admissible("sam/box", "9"));
        }
    }
}

#[test]
fn unplanned_trackers_remain_eligible_without_ready_enrollment() {
    for unlabelled in [false, true] {
        for labels in [
            vec!["tracker"],
            vec!["tracker", "claude"],
            vec!["tracker", "later"],
        ] {
            let (mut b, mut parent, _) = parent_fixture();
            fs::remove_dir_all(b.pass().plan_dir("sam/box", "7")).unwrap();
            b.cfg
                .inbox
                .as_mut()
                .unwrap()
                .triage
                .as_mut()
                .unwrap()
                .sort_unlabelled = unlabelled;
            parent["labels"] = json!(
                labels
                    .iter()
                    .map(|name| json!({"name":name}))
                    .collect::<Vec<_>>()
            );
            fs::write(b.path("repos_sam_box_issues_7"), parent.to_string()).unwrap();
            let t = b.cfg.inbox.as_ref().unwrap().triage.as_ref().unwrap();
            let parked = labels.contains(&"later");
            assert_eq!(
                b.pass().sorted("sam/box", t, &parent, &Default::default()),
                parked
            );
            b.job("planner", "done", "ready");
            let record = b
                .cfg
                .inbox
                .as_ref()
                .unwrap()
                .state
                .join(".triage/sam_box-7");
            fs::create_dir_all(&record).unwrap();
            fs::write(record.join("job"), "planner").unwrap();
            Pass::hand(&record, &b.pass().issue_text("sam/box", &parent, &[])).unwrap();
            b.pass().verdicts("sam/box", t, &[parent]).unwrap();
            assert!(!record.exists());
            assert_eq!(b.log().contains("labels[]=ready"), !parked, "{}", b.log());
        }
    }
}

#[test]
fn split_finalization_establishes_tracker_before_removing_any_enrollment() {
    for label in ["ready", "claude"] {
        let (b, mut parent, list) = parent_fixture();
        parent["labels"] = json!([{"name":label}]);
        fs::write(b.path("repos_sam_box_issues_7"), parent.to_string()).unwrap();
        fs::write(b.path("fail"), "labels[]=tracker").unwrap();
        assert!(!b.pass().split_children("sam/box", "7", &list.tasks));
        assert!(!b.log().contains("DELETE repos/sam/box/issues/7/labels/"));
        let receipts = fs::read(b.pass().plan_dir("sam/box", "7").join("tasks.json")).unwrap();
        fs::remove_file(b.path("fail")).unwrap();
        assert!(b.pass().split_children("sam/box", "7", &list.tasks));
        let log = b.log();
        assert!(
            log.find("labels[]=tracker").unwrap()
                < log.find("DELETE repos/sam/box/issues/7/labels/").unwrap()
        );
        assert_eq!(
            fs::read(b.pass().plan_dir("sam/box", "7").join("tasks.json")).unwrap(),
            receipts
        );
    }
}
