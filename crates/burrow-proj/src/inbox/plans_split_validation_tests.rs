use super::super::sorting_tests::issue;
use super::tests::parent_fixture;
use super::*;
use crate::inbox::record::files;
use crate::planning::Task;

fn task(title: &str, existing: Option<u64>) -> Task {
    Task {
        issue: existing,
        carries: None,
        title: title.into(),
        body: format!("Body for {title}"),
        check: format!("Check for {title}"),
    }
}

#[test]
fn planner_split_with_invalid_existing_issue_rejects_cleanly_without_miss() {
    for case in [
        "ineligible",
        "outside-text",
        "pull-request",
        "missing",
        "self",
    ] {
        let (b, mut parent, _) = parent_fixture();
        parent["labels"] = json!([{"name":"tracker"}]);
        parent["body"] = json!("Updated scope needing replan");
        fs::write(b.path("repos_sam_box_issues_7"), parent.to_string()).unwrap();

        let bad_number = if case == "self" { 7 } else { 99 };
        match case {
            "ineligible" => {
                let bot = issue(99, "github-actions[bot]", &["flaky"]);
                fs::write(b.path("repos_sam_box_issues_99"), bot.to_string()).unwrap();
            }
            "outside-text" => {
                let out = issue(99, "sam", &["outside text"]);
                fs::write(b.path("repos_sam_box_issues_99"), out.to_string()).unwrap();
            }
            "pull-request" => {
                let mut pr = issue(99, "sam", &[]);
                pr["pull_request"] =
                    json!({"url": "https://api.github.com/repos/sam/box/pulls/99"});
                fs::write(b.path("repos_sam_box_issues_99"), pr.to_string()).unwrap();
            }
            "missing" => {
                fs::write(b.path("gone"), "repos/sam/box/issues/99\n").unwrap();
            }
            "self" => {}
            _ => unreachable!(),
        }

        let tasks = vec![
            task("new child first", None),
            task("reused bad child", Some(bad_number)),
        ];
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
        fs::write(record.join(files::File::Job), "planner").unwrap();
        let input = b.pass().issue_text("sam/box", &parent, &[]);
        Pass::hand(&record, &input).unwrap();

        let before_tasks = fs::read(b.pass().plan_dir("sam/box", "7").join("tasks.json")).unwrap();
        let t = b.cfg.inbox.as_ref().unwrap().triage.as_ref().unwrap();
        let mut p = b.pass();
        p.verdicts("sam/box", t, std::slice::from_ref(&parent))
            .unwrap();

        assert!(
            p.unreached.is_empty(),
            "{case}: invalid split must not mark repo unreached: {:?}",
            p.unreached
        );
        assert!(
            !b.log().contains("POST repos/sam/box/issues "),
            "{case}: must not create any child before validating all reused issues:\n{}",
            b.log()
        );
        assert_eq!(
            fs::read(b.pass().plan_dir("sam/box", "7").join("tasks.json")).unwrap(),
            before_tasks,
            "{case}: must not mutate existing tasks.json on invalid split"
        );
        assert!(
            b.posted().contains("Invalid split:")
                && b.posted()
                    .contains("No children created; needs replanning."),
            "{case}: expected invalid split comment, got:\n{}",
            b.posted()
        );
        assert!(
            b.log().contains("labels[]=later"),
            "{case}: expected later label:\n{}",
            b.log()
        );
        assert!(
            !b.log().contains("labels[]=tracker"),
            "{case}: must not label tracker on invalid split:\n{}",
            b.log()
        );
        assert!(!record.exists(), "{case}: sorting record should be cleared");
    }
}

#[test]
fn planner_split_recovers_preexisting_incomplete_tasks_json_keeping_receipts() {
    let (b, mut parent, mut list) = parent_fixture();
    list.children[0].attempted = true;
    assert!(b.pass().save_list("sam/box", "7", &list));
    let child_8 = json!({
        "number": 8,
        "user": burrow_github::fake::actor("workers[bot]"),
        "labels": [],
        "title": "task 0",
        "body": "Body"
    });
    assert!(b.pass().eligible("sam/box", &child_8));

    parent["labels"] = json!([{"name":"tracker"}, {"name":"priority: medium"}]);
    parent["body"] = json!("Updated scope");
    fs::write(b.path("repos_sam_box_issues_7"), parent.to_string()).unwrap();
    fs::write(
        b.path("repos_sam_box_issues_10"),
        issue(10, "sam", &[]).to_string(),
    )
    .unwrap();
    fs::write(
        b.path("repos_sam_box_issues_99"),
        issue(99, "github-actions[bot]", &["flaky"]).to_string(),
    )
    .unwrap();

    let tasks = vec![task("ok existing", Some(10)), task("bot flaky", Some(99))];
    let input = b.pass().issue_text("sam/box", &parent, &[]);
    // Simulate state written by the buggy version before this fix deployed:
    // prepare_plan already moved #8 into retained and wrote the invalid tasks,
    // and split_children partially populated child 0 (#10) before failing on #99.
    assert!(b.pass().prepare_plan("sam/box", "7", &input, &tasks));
    assert!(!b.pass().split_children("sam/box", "7", &tasks));

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
    fs::write(record.join(files::File::Job), "planner").unwrap();
    Pass::hand(&record, &input).unwrap();
    let plan = vec![
        super::super::triage::Step::Priority("priority: medium".into()),
        super::super::triage::Step::Split(tasks),
        super::super::triage::Step::Label("tracker".into()),
    ];
    fs::write(
        record.join(files::File::Plan),
        serde_json::to_vec(&plan).unwrap(),
    )
    .unwrap();
    fs::write(record.join(files::File::Did), "1\n").unwrap();

    let t = b.cfg.inbox.as_ref().unwrap().triage.as_ref().unwrap();
    let mut p = b.pass();
    p.verdicts("sam/box", t, std::slice::from_ref(&parent))
        .unwrap();

    assert!(p.unreached.is_empty(), "{:?}", p.unreached);
    assert!(!record.exists());
    assert!(b.posted().contains("Invalid split:"));
    assert!(b.posted().contains("#99"));
    assert!(b.log().contains("labels[]=later"));
    assert!(
        !b.pass().has_task_list(Some("sam/box"), "7"),
        "incomplete invalid split must be cleared from active tasks"
    );
    assert!(
        !b.pass().plan_dir("sam/box", "7").join("input").exists(),
        "rejected split must not leave parent marked planned on that input"
    );
    assert!(
        b.pass().eligible("sam/box", &child_8),
        "retained creation receipt for #8 must survive recovery"
    );
}

#[test]
fn planner_split_transient_github_error_still_retries_and_recovers() {
    let (b, mut parent, _) = parent_fixture();
    parent["labels"] = json!([{"name":"tracker"}]);
    parent["body"] = json!("Updated scope");
    fs::write(b.path("repos_sam_box_issues_7"), parent.to_string()).unwrap();
    fs::write(
        b.path("repos_sam_box_issues_10"),
        issue(10, "sam", &[]).to_string(),
    )
    .unwrap();
    fs::write(
        b.path("repos_sam_box_issues_11"),
        issue(11, "sam", &[]).to_string(),
    )
    .unwrap();
    fs::write(b.path("fail"), "repos/sam/box/issues/11\n").unwrap();

    let tasks = vec![
        task("first existing", Some(10)),
        task("second existing", Some(11)),
    ];
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
    fs::write(record.join(files::File::Job), "planner").unwrap();
    let input = b.pass().issue_text("sam/box", &parent, &[]);
    Pass::hand(&record, &input).unwrap();

    let t = b.cfg.inbox.as_ref().unwrap().triage.as_ref().unwrap();
    let mut p = b.pass();
    p.verdicts("sam/box", t, std::slice::from_ref(&parent))
        .unwrap();
    assert_eq!(p.unreached, ["sam/box"]);
    assert!(record.exists());
    assert!(!b.posted().contains("Invalid split:"));
    assert!(!b.log().contains("labels[]=later"));

    fs::remove_file(b.path("fail")).unwrap();
    let mut p2 = b.pass();
    p2.verdicts("sam/box", t, std::slice::from_ref(&parent))
        .unwrap();
    assert!(p2.unreached.is_empty());
    assert!(!record.exists());
    assert!(b.log().contains("labels[]=tracker"));
}

#[test]
fn worker_split_with_invalid_existing_issue_rejects_cleanly_without_miss() {
    let (mut b, mut parent, _) = parent_fixture();
    b.cfg.scratch = b.path("scratch");
    b.job_ending("j1", "done", Some("split"), "Splitting work");
    let job = crate::job::Job::new(&b.cfg, "j1");
    let record = b.record("j1");
    parent["labels"] = json!([{"name":"claude"}]);
    fs::write(b.path("repos_sam_box_issues_7"), parent.to_string()).unwrap();
    fs::write(
        b.path("repos_sam_box_issues_99"),
        issue(99, "github-actions[bot]", &["flaky"]).to_string(),
    )
    .unwrap();
    let input = b.pass().issue_text("sam/box", &parent, &[]);
    Pass::hand(&record, &input).unwrap();

    let tasks = vec![task("new child", None), task("bot flaky", Some(99))];
    fs::write(
        job.turn("1", "split"),
        serde_json::to_string(&tasks).unwrap(),
    )
    .unwrap();
    let before_tasks = fs::read(b.pass().plan_dir("sam/box", "7").join("tasks.json")).unwrap();

    // First pass: posting comment succeeds, adding later label hits transient error.
    fs::write(b.path("fail"), "labels[]=later\n").unwrap();
    let mut p1 = b.pass();
    p1.jobs("sam/box", "box", std::slice::from_ref(&parent))
        .unwrap();
    assert_eq!(p1.unreached, ["sam/box"]);
    assert!(record.exists());
    assert_eq!(b.posted().matches("Invalid split:").count(), 1);
    assert!(!b.log().contains("POST repos/sam/box/issues "));
    assert_eq!(
        fs::read(b.pass().plan_dir("sam/box", "7").join("tasks.json")).unwrap(),
        before_tasks
    );

    // Second pass: later label succeeds without double-posting.
    fs::remove_file(b.path("fail")).unwrap();
    let mut p2 = b.pass();
    p2.jobs("sam/box", "box", std::slice::from_ref(&parent))
        .unwrap();
    assert!(p2.unreached.is_empty(), "{:?}", p2.unreached);
    assert_eq!(b.posted().matches("Invalid split:").count(), 1);
    assert!(b.log().contains("labels[]=later"));
    assert!(!record.exists());
    assert!(!job.turn("1", "split").exists());
    assert!(job.turn("1", "split-invalid").exists());
    assert!(!job.path("planning-hold").exists());
    assert!(job.turn("1", "stopped").exists());
}
