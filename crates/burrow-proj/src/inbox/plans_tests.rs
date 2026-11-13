use super::super::sorting_tests::{Box, issue};
use super::*;
use crate::read;

pub(super) fn parent_fixture() -> (Box, Value, List) {
    let mut b = Box::new();
    b.cfg.inbox.as_mut().unwrap().planning = true;
    let mut parent = issue(7, "sam", &["tracker"]);
    parent["state"] = json!("open");
    fs::write(b.path("repos_sam_box_issues_7"), parent.to_string()).unwrap();
    let p = b.pass();
    let source = p.issue_text("sam/box", &parent, &[]);
    p.keep_planned("sam/box", "7", &source).unwrap();
    let mut list = List {
        source: None,
        revision: 0,
        retained: vec![],
        tasks: (0..2)
            .map(|i| Task {
                issue: None,
                title: format!("task {i}"),
                body: "one PR".into(),
                check: "regression".into(),
            })
            .collect(),
        children: vec![
            Child {
                posting_id: Some(burrow_github::fake::author("workers[bot]").id),
                number: Some(8),
                ..Child::default()
            },
            Child {
                posting_id: Some(burrow_github::fake::author("workers[bot]").id),
                number: Some(9),
                ..Child::default()
            },
        ],
        comment: Some(100),
        rendered: String::new(),
    };
    list.rendered = list.checklist("sam/box", "7");
    assert!(p.save_list("sam/box", "7", &list));
    (b, parent, list)
}

#[test]
fn scoped_fetches_check_added_parents() {
    // The event is for child 8, so the saved plan also adds parent 7.
    let (b, mut parent, _) = parent_fixture();
    parent["repository_url"] = json!("https://api.github.com/repos/else/box");
    fs::write(b.path("repos_sam_box_issues_7"), parent.to_string()).unwrap();
    let mut p = b.pass();
    assert!(p.asked("sam/box", BTreeSet::from([8])).is_none());
    assert!(b.log().contains("api repos/sam/box/issues/7"));
    assert!(b.posted().is_empty());
}

#[test]
fn events_drain_one_child_and_only_a_landed_pr_advances_the_parent() {
    let (b, parent, _) = parent_fixture();
    let mut child = issue(8, "sam", &["later"]);
    child["state"] = json!("open");
    fs::write(b.path("repos_sam_box_issues_8"), child.to_string()).unwrap();
    b.pass().sequence("sam/box", std::slice::from_ref(&parent));
    assert!(
        b.log().contains("issues/8/labels -f labels[]=ready"),
        "{}",
        b.log()
    );
    assert!(!b.log().contains("issues/9/labels"));
    assert!(b.pass().child_admissible("sam/box", "8"));
    assert!(!b.pass().child_admissible("sam/box", "9"));
    let ready = |n| {
        let mut issue = super::super::sorting_tests::issue(n, "sam", &["ready"]);
        issue["repository_url"] = json!("https://api.github.com/repos/sam/box");
        issue["created_at"] = json!("2026-11-01T00:00:00Z");
        issue
    };
    let t = b.cfg.inbox.as_ref().unwrap().triage.as_ref().unwrap();
    assert_eq!(b.pass().pick_ready(t, &[ready(9)]), None);
    assert_eq!(
        b.pass().pick_ready(t, &[ready(8)]),
        Some(("sam/box".into(), "8".into()))
    );
    // Closing an issue alone does not release another implementation.
    child["state"] = json!("closed");
    fs::write(b.path("repos_sam_box_issues_8"), child.to_string()).unwrap();
    b.pass().sequence("sam/box", std::slice::from_ref(&parent));
    assert!(!b.pass().child_admissible("sam/box", "9"));
    assert!(!b.pass().child_can_close("sam/box", "8"));
    fs::write(b.path("merged-8"), json!({"data":{"repository":{"issue":{"closedByPullRequestsReferences":{"nodes":[{"merged":true,"url":"https://github.com/sam/box/pull/10"}]}}}}}).to_string()).unwrap();
    b.pass().sequence("sam/box", std::slice::from_ref(&parent));
    assert!(b.pass().child_admissible("sam/box", "9"));
    assert_eq!(
        b.pass().pick_ready(t, &[ready(9)]),
        Some(("sam/box".into(), "9".into()))
    );
    assert!(b.pass().child_can_close("sam/box", "8"));
    let stored: List = serde_json::from_slice(
        &fs::read(b.pass().plan_dir("sam/box", "7").join("tasks.json")).unwrap(),
    )
    .unwrap();
    assert!(stored.checklist("sam/box", "7").contains("[x] #8"));
    assert!(stored.checklist("sam/box", "7").contains("/pull/10"));
}

#[test]
fn edited_and_untrusted_parents_cannot_admit_children() {
    for change in ["edit", "outside", "opener"] {
        let (b, mut parent, _) = parent_fixture();
        match change {
            "edit" => parent["body"] = json!("Changed scope"),
            "outside" => parent["labels"] = json!([{"name":"outside text"}]),
            _ => parent["user"] = burrow_github::fake::actor("stranger"),
        }
        fs::write(b.path("repos_sam_box_issues_7"), parent.to_string()).unwrap();
        assert!(!b.pass().child_admissible("sam/box", "8"), "{change}");
    }
}

#[test]
fn a_partial_split_never_marks_its_parent_ready_for_implementation() {
    let (b, parent, _) = parent_fixture();
    assert!(!b.pass().planned(Some("sam/box"), &parent));
}
#[test]
fn a_deferred_child_is_not_repeatedly_released_and_replanned() {
    let (b, parent, _) = parent_fixture();
    let mut child = issue(8, "sam", &["later"]);
    child["state"] = json!("open");
    fs::write(b.path("repos_sam_box_issues_8"), child.to_string()).unwrap();
    b.pass().sequence("sam/box", std::slice::from_ref(&parent));
    b.pass().sequence("sam/box", &[parent]);
    assert_eq!(
        b.log().matches("issues/8/labels -f labels[]=ready").count(),
        1
    );
}
#[test]
fn an_exact_child_keeps_its_parent_size_approval_without_another_planner() {
    let (b, _parent, list) = parent_fixture();
    for (i, task) in list.tasks.iter().enumerate() {
        let mut child = issue(8 + i as u64, "sam", &["ready"]);
        child["title"] = json!(task.title);
        child["body"] = json!(Pass::child_body("sam/box", "7", i, task));
        fs::write(
            b.path(&format!("repos_sam_box_issues_{}", 8 + i)),
            child.to_string(),
        )
        .unwrap();
    }
    assert!(b.pass().split_children("sam/box", "7", &list.tasks));
    let child: Value =
        serde_json::from_slice(&fs::read(b.path("repos_sam_box_issues_8")).unwrap()).unwrap();
    assert!(b.pass().planned(Some("sam/box"), &child));
}

#[test]
fn lost_creation_answers_are_reconciled_without_another_child_post() {
    let (b, _parent, mut list) = parent_fixture();
    for child in &mut list.children {
        child.number = None;
        child.attempted = true;
    }
    list.rendered.clear();
    assert!(b.pass().save_list("sam/box", "7", &list));
    let found: Vec<_> = (0..2)
        .map(|i| {
            let mut child = issue(8 + i, "sam", &["later"]);
            child["body"] = json!(format!(
                "task body\n<!-- proj-child sam/box#7 {} -->",
                i + 1
            ));
            child
        })
        .collect();
    fs::write(b.path("repos_sam_box_issues"), json!(found).to_string()).unwrap();
    assert!(b.pass().split_children("sam/box", "7", &list.tasks));
    assert!(b.pass().split_children("sam/box", "7", &list.tasks));
    assert!(
        !b.log().contains("-X POST repos/sam/box/issues "),
        "{}",
        b.log()
    );
    assert_eq!(
        b.log()
            .matches("-X PATCH repos/sam/box/issues/comments/100")
            .count(),
        1
    );
}
#[test]
fn a_worker_split_posts_its_answer_before_retiring_and_can_retry_a_partial_plan() {
    let (b, parent, list) = parent_fixture();
    b.job_ending("j1", "done", Some("split"), "Split into bounded tasks.");
    let record = b.record("j1");
    fs::write(
        b.cfg.jobs.join("j1/1.split"),
        serde_json::to_string(&list.tasks).unwrap(),
    )
    .unwrap();
    let mut enrolled = parent;
    enrolled["labels"] = json!([{"name":"claude"}]);
    enrolled["repository_url"] = json!("https://api.github.com/repos/sam/box");
    b.pass()
        .jobs("sam/box", "box", std::slice::from_ref(&enrolled))
        .unwrap();
    assert!(!record.exists());
    assert!(b.posted().contains("Split into bounded tasks."));
    assert!(!b.pass().worker_split_pending(Some("sam/box"), "7"));
    assert!(b.log().contains("labels[]=tracker"));
    let t = b.cfg.inbox.as_ref().unwrap().triage.as_ref().unwrap();
    enrolled["labels"] = json!([{"name":"tracker"}]);
    assert!(
        b.pass()
            .sorted("sam/box", t, &enrolled, &Default::default())
    );
    enrolled["body"] = json!("changed after retirement");
    fs::write(b.path("repos_sam_box_issues_7"), enrolled.to_string()).unwrap();
    assert!(
        !b.pass()
            .sorted("sam/box", t, &enrolled, &Default::default()),
        "a retired worker's marker cannot prevent replanning changed scope"
    );
    assert!(!b.log().contains("-X POST repos/sam/box/issues "));
}
#[test]
fn a_human_claude_issue_is_sized_on_a_fresh_codex_job_before_implementation() {
    let mut b = Box::new();
    b.cfg.inbox.as_mut().unwrap().planning = true;
    b.cfg.projects = b.path("projects");
    fs::create_dir_all(b.cfg.projects.join("box")).unwrap();
    b.cfg.tools.systemd_run = "true".into();
    b.cfg.tools.systemctl = "false".into();
    b.cfg
        .inbox
        .as_mut()
        .unwrap()
        .triage
        .as_mut()
        .unwrap()
        .sort_unlabelled = false;
    let mut issue = issue(7, "sam", &["claude"]);
    issue["repository_url"] = json!("https://api.github.com/repos/sam/box");
    b.pass()
        .triage(
            "sam/box",
            &[issue, super::super::sorting_tests::issue(8, "sam", &[])],
        )
        .unwrap();
    let record = b
        .cfg
        .inbox
        .as_ref()
        .unwrap()
        .state
        .join(".triage/sam_box-7");
    let id = read(&record.join("job")).expect("a size-planning job");
    let job = crate::job::Job::new(&b.cfg, &id);
    assert_eq!(
        crate::engine::choice(&b.cfg, &job).unwrap().engine,
        crate::engine::Engine::Codex
    );
    assert!(
        !b.cfg
            .inbox
            .as_ref()
            .unwrap()
            .state
            .join(".triage/sam_box-8")
            .exists()
    );
    assert!(!job.path("session").exists());
    assert!(
        read(&job.path("prompt.1"))
            .unwrap()
            .contains("Plan the size")
    );
    assert!(
        !b.cfg
            .inbox
            .as_ref()
            .unwrap()
            .state
            .join("sam_box-7/job")
            .exists()
    );
}
#[test]
fn existing_cutoff_recovery_and_answer_accounting_are_preserved() {
    let (mut b, mut issue, _) = parent_fixture();
    b.cfg.tools.systemctl = "false".into();
    b.cfg.tools.systemd_run = "true".into();
    fs::remove_file(b.pass().plan_dir("sam/box", "7").join("tasks.json")).unwrap();
    b.job_ending("j1", "done", None, "");
    let record = b.record("j1");
    let job = crate::job::Job::new(&b.cfg, "j1");
    fs::remove_file(job.turn("1", "outcome")).unwrap();
    fs::write(
        job.turn("1", "jsonl"),
        "{\"type\":\"system\",\"session_id\":\"s\"}\n",
    )
    .unwrap();
    fs::write(job.path("session"), "s").unwrap();
    assert!(job.cut_off("1"));
    issue["labels"] = json!([{"name":"claude"}]);
    fs::write(b.path("repos_sam_box_issues_7"), issue.to_string()).unwrap();
    b.pass()
        .jobs("sam/box", "box", std::slice::from_ref(&issue))
        .unwrap();
    assert_eq!(job.turns().as_deref(), Some("2"));
    assert!(read(&job.path("prompt.2")).unwrap().contains("cut off"));
    assert_eq!(read(&record.join("answered")).as_deref(), Some("1"));
}
#[test]
fn existing_workers_are_not_resized_on_every_reply() {
    let mut b = Box::new();
    b.cfg.inbox.as_mut().unwrap().planning = true;
    b.job("j1", "done", "unfinished PR #4");
    b.record("j1");
    let mut issue = issue(7, "sam", &["claude"]);
    issue["repository_url"] = json!("https://api.github.com/repos/sam/box");
    assert!(b.pass().started_issue_job(Some("sam/box"), "7"));
    let t = b.cfg.inbox.as_ref().unwrap().triage.as_ref().unwrap();
    assert!(b.pass().sorted("sam/box", t, &issue, &Default::default()));
}
#[test]
fn a_parent_advances_only_past_a_recorded_merged_pr() {
    let mut list = List {
        source: None,
        revision: 0,
        retained: vec![],
        tasks: vec![],
        children: vec![Child::default(); 100],
        comment: None,
        rendered: String::new(),
    };
    for n in 0..100 {
        assert_eq!(list.next(), Some(n));
        list.children[n].number = Some(n as u64 + 1);
        assert_eq!(
            list.next(),
            Some(n),
            "an issue number alone is not completion"
        );
        list.children[n].merged_pr = Some(format!("https://github.com/o/r/pull/{n}"));
        let disk = serde_json::to_string(&list).unwrap();
        list = serde_json::from_str(&disk).unwrap();
    }
    assert_eq!(list.next(), None);
}

#[test]
fn tracker_split_transactions_recover_every_durable_stage() {
    for enrollment in ["tracker", "ready", "claude"] {
        for stage in [
            "creation",
            "receipt",
            "approval",
            "checklist",
            "label",
            "did",
        ] {
            let (b, mut parent, mut list) = parent_fixture();
            parent["labels"] = json!([{"name":enrollment}]);
            parent["repository_url"] = json!("https://api.github.com/repos/sam/box");
            fs::write(b.path("repos_sam_box_issues_7"), parent.to_string()).unwrap();
            let tasks = list.tasks.clone();
            let answer = format!("split\n{}", serde_json::to_string(&tasks).unwrap());
            b.job("planner", "done", &answer);
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
            let steps = vec![
                super::super::triage::Step::Split(tasks.clone()),
                super::super::triage::Step::Label("tracker".into()),
            ];
            fs::write(record.join("plan"), serde_json::to_string(&steps).unwrap()).unwrap();
            let mut children = Vec::new();
            for (i, task) in tasks.iter().enumerate() {
                let mut child = issue(8 + i as u64, "sam", &["later"]);
                child["title"] = json!(task.title);
                child["body"] = json!(Pass::child_body("sam/box", "7", i, task));
                fs::write(
                    b.path(&format!("repos_sam_box_issues_{}", 8 + i)),
                    child.to_string(),
                )
                .unwrap();
                children.push(child);
            }
            if stage == "creation" || stage == "receipt" {
                list.children[1].number = None;
                list.children[1].attempted = stage == "receipt";
            }
            fs::write(b.path("repos_sam_box_issues"), json!(children).to_string()).unwrap();
            if stage == "checklist" {
                list.rendered.clear();
            }
            if stage == "label" || stage == "did" {
                fs::write(record.join("did"), "1").unwrap();
            }
            assert!(b.pass().save_list("sam/box", "7", &list));
            let t = b.cfg.inbox.as_ref().unwrap().triage.as_ref().unwrap();
            assert!(
                !b.pass().sorted("sam/box", t, &parent, &Default::default()),
                "{stage}"
            );
            b.pass().verdicts("sam/box", t, &[parent]).unwrap();
            assert!(!record.exists(), "{stage}: {}", b.log());
            if stage != "label" && stage != "did" {
                assert!(
                    b.pass().plan_dir("sam/box", "8").join("input").exists(),
                    "{stage}"
                );
                let saved: List = serde_json::from_slice(
                    &fs::read(b.pass().plan_dir("sam/box", "7").join("tasks.json")).unwrap(),
                )
                .unwrap();
                assert!(saved.children.iter().all(|c| c.number.is_some()), "{stage}");
            }
            assert!(b.log().contains("labels[]=tracker"), "{stage}");
            assert!(
                !b.log().contains("-X POST repos/sam/box/issues "),
                "{stage}"
            );
        }
    }
}

#[test]
fn planning_preserves_queued_running_and_awaiting_reference_ownership() {
    for state in ["queued", "running", "awaiting"] {
        let (b, mut parent, _) = parent_fixture();
        parent["repository_url"] = json!("https://api.github.com/repos/sam/box");
        b.job("lane", "done", "working");
        let record = b.record("lane");
        Pass::hand(&record, "Deliver #7").unwrap();
        let lane = record.with_file_name("sam_box-100");
        fs::rename(record, &lane).unwrap();
        fs::write(lane.join("issue"), "sam/box#100").unwrap();
        if state == "awaiting" {
            fs::write(b.cfg.jobs.join("lane/await"), "sam/box#99\t0\told\n").unwrap();
        } else {
            fs::write(b.path("active"), "").unwrap();
            if state == "running" {
                fs::write(b.cfg.jobs.join("lane/1.slot"), "").unwrap();
            }
        }
        let t = b.cfg.inbox.as_ref().unwrap().triage.as_ref().unwrap();
        let mut pass = b.pass();
        let task_file = pass.plan_dir("sam/box", "7").join("tasks.json");
        let tasks = fs::read(&task_file).unwrap();
        fs::remove_file(&task_file).unwrap();
        assert!(
            pass.sorted("sam/box", t, &parent, &pass.claimed("sam/box")),
            "{state}"
        );
        fs::write(task_file, tasks).unwrap();
        assert!(!pass.child_admissible("sam/box", "8"), "{state}");
        pass.sequence("sam/box", &[parent]);
        assert!(!b.log().contains("labels[]=ready"), "{state}");
    }
}

#[test]
fn approvals_compare_exact_task_bytes_for_bodies_comments_and_children() {
    for suffix in [" ", "\t", "\n", " \t\n\n"] {
        for comment in [false, true] {
            let (b, mut parent, _) = parent_fixture();
            fs::remove_file(b.pass().plan_dir("sam/box", "7").join("tasks.json")).unwrap();
            let mut comments = Vec::new();
            if comment {
                comments
                    .push(json!({"id":1,"user":burrow_github::fake::actor("sam"),"body":format!("body{suffix}")}));
            } else {
                parent["body"] = json!(format!("body{suffix}"));
            }
            fs::write(b.path("repos_sam_box_issues_7"), parent.to_string()).unwrap();
            fs::write(b.path("comments"), json!([comments]).to_string()).unwrap();
            let source = b.pass().input("sam/box", &parent).unwrap();
            b.pass().keep_planned("sam/box", "7", &source).unwrap();
            assert!(
                b.pass().planned(Some("sam/box"), &parent),
                "{suffix:?} {comment}"
            );
            if comment {
                fs::write(
                    b.path("comments"),
                    json!([[{"id":1,"user":burrow_github::fake::actor("sam"),"body":format!("body{suffix} ")}]])
                        .to_string(),
                )
                .unwrap();
            } else {
                parent["body"] = json!(format!("body{suffix} "));
                fs::write(b.path("repos_sam_box_issues_7"), parent.to_string()).unwrap();
            }
            assert!(
                !b.pass().planned(Some("sam/box"), &parent),
                "{suffix:?} {comment}"
            );
        }
        let (b, _, mut list) = parent_fixture();
        for (i, task) in list.tasks.iter_mut().enumerate() {
            task.issue = Some(8 + i as u64);
            task.body.push_str(suffix);
            let mut child = issue(8 + i as u64, "sam", &["ready"]);
            child["title"] = json!(task.title);
            child["body"] = json!(task.body);
            fs::write(
                b.path(&format!("repos_sam_box_issues_{}", 8 + i)),
                child.to_string(),
            )
            .unwrap();
        }
        assert!(b.pass().save_list("sam/box", "7", &list));
        assert!(b.pass().split_children("sam/box", "7", &list.tasks));
        let child: Value =
            serde_json::from_slice(&fs::read(b.path("repos_sam_box_issues_8")).unwrap()).unwrap();
        assert!(
            b.pass().planned(Some("sam/box"), &child),
            "reused child {suffix:?}"
        );
    }
}

#[test]
fn generated_child_approval_detects_every_trailing_whitespace_edit() {
    for suffix in [" ", "\t", "\n", " \t\n\n"] {
        let (b, _, list) = parent_fixture();
        for (i, task) in list.tasks.iter().enumerate() {
            let mut child = issue(8 + i as u64, "sam", &["later"]);
            child["title"] = json!(task.title);
            child["body"] = json!(Pass::child_body("sam/box", "7", i, task));
            fs::write(
                b.path(&format!("repos_sam_box_issues_{}", 8 + i)),
                child.to_string(),
            )
            .unwrap();
        }
        assert!(b.pass().split_children("sam/box", "7", &list.tasks));
        let mut child: Value =
            serde_json::from_slice(&fs::read(b.path("repos_sam_box_issues_8")).unwrap()).unwrap();
        assert!(b.pass().planned(Some("sam/box"), &child));
        child["body"] = json!(format!("{}{suffix}", child["body"].as_str().unwrap()));
        fs::write(b.path("repos_sam_box_issues_8"), child.to_string()).unwrap();
        assert!(
            !b.pass().planned(Some("sam/box"), &child),
            "body {suffix:?}"
        );

        // A newly approved comment must retain its terminal newlines too.
        fs::write(
            b.path("comments"),
            json!([[{"id":1,"user":burrow_github::fake::actor("sam"),"body":"scope"}]]).to_string(),
        )
        .unwrap();
        let source = b.pass().input("sam/box", &child).unwrap();
        b.pass().keep_planned("sam/box", "8", &source).unwrap();
        assert!(b.pass().planned(Some("sam/box"), &child));
        fs::write(
            b.path("comments"),
            json!([[{"id":1,"user":burrow_github::fake::actor("sam"),"body":format!("scope{suffix}")}]]).to_string(),
        )
        .unwrap();
        assert!(
            !b.pass().planned(Some("sam/box"), &child),
            "comment {suffix:?}"
        );
    }
}

#[test]
fn unavailable_planning_reads_retry_without_false_stale_notices() {
    for endpoint in ["source", "parent", "completion"] {
        let (b, parent, _) = parent_fixture();
        let mut child = issue(8, "sam", &["later"]);
        child["state"] = json!("closed");
        fs::write(b.path("repos_sam_box_issues_8"), child.to_string()).unwrap();
        let failure = match endpoint {
            "source" => "api graphql",
            "parent" => "api repos/sam/box/issues/7",
            _ => "merged-query",
        };
        fs::write(b.path("fail"), failure).unwrap();
        let mut pass = b.pass();
        if endpoint == "parent" {
            assert!(!pass.child_admissible("sam/box", "8"));
        } else {
            pass.sequence("sam/box", std::slice::from_ref(&parent));
        }
        assert!(pass.wake.contains_key("sam/box"), "{endpoint}");
        assert!(pass.unreached.contains(&"sam/box".to_owned()), "{endpoint}");
        assert!(!pass.plan_dir("sam/box", "7").join("stale-notice").exists());
        assert!(!b.posted().contains("parent task changed"));
        fs::remove_file(b.path("fail")).unwrap();
        assert!(b.pass().child_admissible("sam/box", "8"));
    }
}

#[test]
fn unstick_preflight_bypasses_ordinary_triage_admission() {
    for engine in ["claude", "codex"] {
        let mut b = Box::new();
        let dir = b.path("settings");
        crate::config::tests::set(
            &mut b.cfg,
            &dir,
            &[
                &format!("role planner {engine}"),
                &format!("offer {engine} models selected"),
                &format!("model planner {engine} selected"),
            ],
        );
        b.cfg.inbox.as_mut().unwrap().planning = true;
        for n in [8, 9] {
            let id = format!("busy{n}");
            b.job(&id, "done", "awaiting sorting");
            let record = b
                .cfg
                .inbox
                .as_ref()
                .unwrap()
                .state
                .join(format!(".triage/other_repo-{n}"));
            fs::create_dir_all(&record).unwrap();
            fs::write(record.join("job"), id).unwrap();
        }
        assert_eq!(
            b.cfg
                .inbox
                .as_ref()
                .unwrap()
                .triage
                .as_ref()
                .unwrap()
                .at_once,
            2
        );
        b.cfg.projects = b.path("projects");
        fs::create_dir_all(b.cfg.projects.join("box")).unwrap();
        b.cfg.tools.systemd_run = "true".into();
        b.cfg.tools.systemctl = "false".into();
        let mut task = issue(
            7,
            "sam",
            &["claude", &b.cfg.inbox.as_ref().unwrap().unstick_label],
        );
        task["repository_url"] = json!("https://api.github.com/repos/sam/box");
        b.pass().triage("sam/box", &[task]).unwrap();
        let id = read(
            &b.cfg
                .inbox
                .as_ref()
                .unwrap()
                .state
                .join(".triage/sam_box-7/job"),
        )
        .unwrap();
        let job = crate::job::Job::new(&b.cfg, &id);
        assert!(job.path("unstick").exists());
        assert_eq!(read(&job.path("role")).as_deref(), Some("planner"));
        assert_eq!(
            crate::engine::choice(&b.cfg, &job).unwrap().engine.name(),
            engine
        );
    }
}

#[test]
fn a_cutoff_split_intent_is_recovered_without_an_end_declaration_or_another_turn() {
    let (b, mut parent, list) = parent_fixture();
    b.job_ending("j1", "error", None, "");
    let record = b.record("j1");
    let job = crate::job::Job::new(&b.cfg, "j1");
    fs::remove_file(job.turn("1", "outcome")).unwrap();
    fs::write(
        job.turn("1", "jsonl"),
        "{\"type\":\"system\",\"session_id\":\"s\"}\n",
    )
    .unwrap();
    fs::write(job.path("session"), "s").unwrap();
    assert!(job.cut_off("1"));
    // Crash at the first durable write: no planning-hold or end.next exists yet.
    fs::write(
        job.turn("1", "split"),
        serde_json::to_string(&list.tasks).unwrap(),
    )
    .unwrap();
    parent["labels"] = json!([{"name":"claude"}]);
    parent["body"] = json!("worker scope \t\n\n");
    fs::write(b.path("repos_sam_box_issues_7"), parent.to_string()).unwrap();
    Pass::hand(&record, &b.pass().issue_text("sam/box", &parent, &[])).unwrap();
    b.pass()
        .jobs("sam/box", "box", std::slice::from_ref(&parent))
        .unwrap();
    assert_eq!(b.pass().same_source("sam/box", &parent), Some(true));
    assert_eq!(job.turns().as_deref(), Some("1"));
    assert!(!record.exists());
    assert!(job.path("planning-hold").exists());
    assert!(!b.pass().worker_split_pending(Some("sam/box"), "7"));
    assert!(b.log().contains("labels[]=tracker"));
}

#[test]
fn unavailable_approval_verification_cannot_admit_an_implementation() {
    let (b, parent, _) = parent_fixture();
    fs::remove_file(b.pass().plan_dir("sam/box", "7").join("tasks.json")).unwrap();
    fs::write(b.path("fail"), "api graphql").unwrap();
    let mut pass = b.pass();
    assert!(!pass.planned(Some("sam/box"), &parent));
    assert!(pass.wake.contains_key("sam/box"));
    assert!(!pass.plan_dir("sam/box", "7").join("stale-notice").exists());
}

#[test]
fn a_ready_verdict_retains_trailing_task_whitespace_exactly() {
    let mut b = Box::new();
    b.cfg.inbox.as_mut().unwrap().planning = true;
    let mut parent = issue(7, "sam", &["ready"]);
    parent["body"] = json!("task \t\n\n");
    parent["repository_url"] = json!("https://api.github.com/repos/sam/box");
    fs::write(b.path("repos_sam_box_issues_7"), parent.to_string()).unwrap();
    b.job("planner", "done", "ready");
    let d = b
        .cfg
        .inbox
        .as_ref()
        .unwrap()
        .state
        .join(".triage/sam_box-7");
    fs::create_dir_all(&d).unwrap();
    fs::write(d.join("job"), "planner").unwrap();
    Pass::hand(&d, &b.pass().issue_text("sam/box", &parent, &[])).unwrap();
    let t = b.cfg.inbox.as_ref().unwrap().triage.as_ref().unwrap();
    b.pass()
        .verdicts("sam/box", t, std::slice::from_ref(&parent))
        .unwrap();
    assert!(b.pass().planned(Some("sam/box"), &parent));
}

#[test]
fn unlisted_repositories_sort_but_never_offer_children() {
    let (mut b, parent, _) = parent_fixture();
    b.cfg.settings = Some(burrow_settings::Config {
        overlay: b.path("settings-overlay.json"),
        ..Default::default()
    });
    let before = b.log();
    b.pass().sequence("sam/box", &[parent]);
    assert_eq!(b.log(), before, "no child handoff in an unlisted repo");
    let mut pass = b.pass();
    pass.triage("sam/box", &[issue(99, "sam", &[])]).unwrap();
    assert!(
        pass.wake.contains_key("sam/box"),
        "sorting can still attempt a planner session"
    );
}

// The live snapshot's queue is the plan's order alone: an observing pass asks
// GitHub nothing about a child or its parents, where the drain's own pass,
// which hands one on, still does.
#[test]
fn the_snapshot_queue_asks_github_nothing() {
    let (b, _, _) = parent_fixture();
    let ready = |n| {
        let mut issue = super::super::sorting_tests::issue(n, "sam", &["ready"]);
        issue["repository_url"] = json!("https://api.github.com/repos/sam/box");
        issue["created_at"] = json!("2026-11-01T00:00:00Z");
        issue
    };
    b.pass()
        .note_ready("sam/box", &[ready(8), ready(9)])
        .unwrap();
    let t = b.cfg.inbox.as_ref().unwrap().triage.clone().unwrap();
    let before = b.log();
    let mut observing = b.pass();
    observing.observe = true;
    let queue: Vec<u64> = observing
        .view(&t, crate::now())
        .queue
        .iter()
        .map(|r| r.number)
        .collect();
    assert_eq!(queue, [8], "the plan's next child, and not the one after");
    assert_eq!(b.log(), before, "the snapshot asked GitHub");
    // The drain's pass checks the parent on GitHub before it hands one on.
    assert_eq!(
        b.pass().pick_ready(&t, &[ready(8)]),
        Some(("sam/box".into(), "8".into()))
    );
    assert!(
        b.log().len() > before.len(),
        "the drain asked GitHub nothing"
    );
}
