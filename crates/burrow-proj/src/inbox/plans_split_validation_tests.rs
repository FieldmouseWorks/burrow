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

    // Persist GitHub's accepted comment from pass 1; pass 2 must reconcile it
    // from GitHub without relying on a local posted marker.
    let accepted = json!([[{
        "id": 500,
        "user": burrow_github::fake::actor("sam"),
        "body": b.posted(),
        "created_at": "2026-11-25T22:00:00Z"
    }]]);
    fs::write(
        b.path("repos_sam_box_issues_7_comments"),
        accepted.to_string(),
    )
    .unwrap();

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

#[test]
fn worker_split_invalid_comment_reconciles_existing_github_comment_without_duplicate() {
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

    // Simulate a crash after turn 1's answer and GitHub's invalid-split
    // comment were posted, before any local split-rejection state was updated.
    files::mark(&record, files::File::Answered)
        .put("1\n")
        .unwrap();
    let marker = invalid_split_marker("sam/box", "7", "j1", Some("1"));
    let existing = json!([[{
        "id": 501,
        "user": burrow_github::fake::actor("sam"),
        "body": format!("Invalid split: already posted\n\n{marker}\n\n---\n{}\n_proj-inbox on box_\n", b.cfg.inbox_mark),
        "created_at": "2026-11-25T22:00:00Z"
    }]]);
    fs::write(
        b.path("repos_sam_box_issues_7_comments"),
        existing.to_string(),
    )
    .unwrap();

    let mut p = b.pass();
    p.jobs("sam/box", "box", std::slice::from_ref(&parent))
        .unwrap();
    assert!(p.unreached.is_empty(), "{:?}", p.unreached);
    assert!(
        b.posted().is_empty(),
        "must not re-post already-accepted invalid-split comment:\n{}",
        b.posted()
    );
    assert!(b.log().contains("labels[]=later"));
    assert!(!record.exists());
    assert!(!job.turn("1", "split").exists());
    assert!(job.turn("1", "split-invalid").exists());
}

#[test]
fn planner_split_invalid_comment_reconciles_existing_github_comment_without_duplicate() {
    let (b, mut parent, _) = parent_fixture();
    parent["labels"] = json!([{"name":"tracker"}]);
    parent["body"] = json!("Updated scope");
    fs::write(b.path("repos_sam_box_issues_7"), parent.to_string()).unwrap();
    fs::write(
        b.path("repos_sam_box_issues_99"),
        issue(99, "github-actions[bot]", &["flaky"]).to_string(),
    )
    .unwrap();

    let tasks = vec![task("new child", None), task("bot flaky", Some(99))];
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

    let marker = invalid_split_marker("sam/box", "7", "planner", None);
    let existing = json!([[{
        "id": 502,
        "user": burrow_github::fake::actor("sam"),
        "body": format!("**claude job**\n\nInvalid split: already posted\n\n{marker}\n\n---\n{}\n_proj-inbox on box_\n", b.cfg.inbox_mark),
        "created_at": "2026-11-25T22:00:00Z"
    }]]);
    fs::write(
        b.path("repos_sam_box_issues_7_comments"),
        existing.to_string(),
    )
    .unwrap();

    let t = b.cfg.inbox.as_ref().unwrap().triage.as_ref().unwrap();
    let mut p = b.pass();
    p.verdicts("sam/box", t, std::slice::from_ref(&parent))
        .unwrap();
    assert!(p.unreached.is_empty(), "{:?}", p.unreached);
    assert!(
        b.posted().is_empty(),
        "must not re-post already-accepted planner invalid-split comment:\n{}",
        b.posted()
    );
    assert!(b.log().contains("labels[]=later"));
    assert!(!b.log().contains("labels[]=tracker"));
    assert!(!record.exists());
}

#[test]
fn invalid_split_recovery_reports_retained_partial_children_without_claiming_none_created() {
    // Worker split recovering an incomplete tasks.json with one created child
    // (#100) and one unconfirmed creation attempt before the invalid #99.
    {
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

        let tasks = vec![
            task("already created child", None),
            task("ambiguous child", None),
            task("bot flaky", Some(99)),
        ];
        fs::write(
            job.turn("1", "split"),
            serde_json::to_string(&tasks).unwrap(),
        )
        .unwrap();
        assert!(b.pass().prepare_plan("sam/box", "7", &input, &tasks));
        let path = b.pass().plan_dir("sam/box", "7").join("tasks.json");
        let mut list: List = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        list.children[0].attempted = true;
        list.children[0].number = Some(100);
        list.children[1].attempted = true;
        list.children[1].number = None;
        assert!(b.pass().save_list("sam/box", "7", &list));

        let mut p = b.pass();
        p.jobs("sam/box", "box", std::slice::from_ref(&parent))
            .unwrap();
        assert!(p.unreached.is_empty(), "{:?}", p.unreached);
        let posted = b.posted();
        assert!(posted.contains("Invalid split:"), "{posted}");
        assert!(
            !posted.contains("No children created"),
            "must not claim no children were created when partial receipts were retained:\n{posted}"
        );
        assert!(
            posted.contains("Retained partial child #100 and 1 unconfirmed child-creation attempt"),
            "{posted}"
        );
        assert!(
            posted.contains("no further children will be created; needs replanning."),
            "{posted}"
        );
        let after: List = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert!(after.tasks.is_empty());
        assert!(after.retained.iter().any(|r| r.child.number == Some(100)));
        assert!(
            after
                .retained
                .iter()
                .any(|r| r.child.attempted && r.child.number.is_none())
        );
    }

    // Planner split recovering an incomplete tasks.json with a created child (#100).
    {
        let (b, mut parent, _) = parent_fixture();
        parent["labels"] = json!([{"name":"tracker"}]);
        parent["body"] = json!("Updated scope");
        fs::write(b.path("repos_sam_box_issues_7"), parent.to_string()).unwrap();
        fs::write(
            b.path("repos_sam_box_issues_99"),
            issue(99, "github-actions[bot]", &["flaky"]).to_string(),
        )
        .unwrap();

        let tasks = vec![
            task("already created child", None),
            task("bot flaky", Some(99)),
        ];
        let input = b.pass().issue_text("sam/box", &parent, &[]);
        assert!(b.pass().prepare_plan("sam/box", "7", &input, &tasks));
        let path = b.pass().plan_dir("sam/box", "7").join("tasks.json");
        let mut list: List = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        list.children[0].attempted = true;
        list.children[0].number = Some(100);
        assert!(b.pass().save_list("sam/box", "7", &list));

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
        let posted = b.posted();
        assert!(posted.contains("Invalid split:"), "{posted}");
        assert!(
            !posted.contains("No children created"),
            "planner recovery must not claim no children were created:\n{posted}"
        );
        assert!(posted.contains("Retained partial child #100"), "{posted}");
        assert!(
            posted.contains("no further children will be created; needs replanning."),
            "{posted}"
        );
        let after: List = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert!(after.tasks.is_empty());
        assert!(after.retained.iter().any(|r| r.child.number == Some(100)));
    }
}

#[test]
fn abort_incomplete_split_removes_input_before_emptying_list_and_retries_on_failure() {
    // 1. If removing `input` fails, abort_incomplete_split must return false
    // without emptying tasks.json so has_task_list keeps blocking admission.
    {
        let (b, parent, _) = parent_fixture();
        let input = b.pass().issue_text("sam/box", &parent, &[]);
        let tasks = vec![task("first", None), task("bot flaky", Some(99))];
        assert!(b.pass().prepare_plan("sam/box", "7", &input, &tasks));

        let d = b.pass().plan_dir("sam/box", "7");
        let before_tasks = fs::read(d.join("tasks.json")).unwrap();
        let input_path = d.join("input");
        fs::remove_file(&input_path).unwrap();
        fs::create_dir(&input_path).unwrap();

        assert!(
            !b.pass()
                .abort_incomplete_split("sam/box", "7", &input, &tasks),
            "non-NotFound input removal failure must trigger retry"
        );
        assert!(
            b.pass().has_task_list(Some("sam/box"), "7"),
            "nonempty task list must stay intact while input removal fails"
        );
        assert_eq!(
            fs::read(d.join("tasks.json")).unwrap(),
            before_tasks,
            "tasks.json must not be modified before input is removed"
        );

        // Once input can be removed, retrying abort_incomplete_split succeeds.
        fs::remove_dir(&input_path).unwrap();
        fs::write(&input_path, &input).unwrap();
        assert!(
            b.pass()
                .abort_incomplete_split("sam/box", "7", &input, &tasks)
        );
        assert!(!input_path.exists());
        assert!(!b.pass().has_task_list(Some("sam/box"), "7"));
        assert!(!b.pass().approved_input("sam/box", "7", &input));
    }

    // 2. `input` must be removed before `save_list` writes the emptied list:
    // if saving `tasks.json` fails (or crashes), `input` is already gone,
    // `has_task_list` still blocks admission, and a retry finishes cleanly.
    {
        let (b, parent, _) = parent_fixture();
        let input = b.pass().issue_text("sam/box", &parent, &[]);
        let tasks = vec![
            task("already created child", None),
            task("bot flaky", Some(99)),
        ];
        assert!(b.pass().prepare_plan("sam/box", "7", &input, &tasks));

        let d = b.pass().plan_dir("sam/box", "7");
        let path = d.join("tasks.json");
        let mut list: List = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        list.children[0].attempted = true;
        list.children[0].number = Some(100);
        assert!(b.pass().save_list("sam/box", "7", &list));

        let swap_blocker = d.join("tasks.json.new");
        fs::create_dir(&swap_blocker).unwrap();

        assert!(
            !b.pass()
                .abort_incomplete_split("sam/box", "7", &input, &tasks),
            "save_list failure must trigger retry"
        );
        assert!(
            !d.join("input").exists(),
            "input approval must already be removed before save_list runs"
        );
        assert!(b.pass().has_task_list(Some("sam/box"), "7"));
        assert!(!b.pass().approved_input("sam/box", "7", &input));

        // Retry after save_list can succeed: NotFound on `input` is fine and
        // partial receipts are retained.
        fs::remove_dir(&swap_blocker).unwrap();
        assert!(
            b.pass()
                .abort_incomplete_split("sam/box", "7", &input, &tasks)
        );
        assert!(!b.pass().has_task_list(Some("sam/box"), "7"));
        assert!(!b.pass().approved_input("sam/box", "7", &input));
        let after: List = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert!(after.tasks.is_empty());
        assert_eq!(after.source, None);
        assert!(after.retained.iter().any(|r| r.child.number == Some(100)));
    }
}

#[test]
fn post_binding_split_retry_aborts_and_reports_retained_children_without_sequencing() {
    // 1. Planner split: pass 1 created #100 and bound reused #99, then hit a
    // transient error in a post-binding step. Before pass 2, #99 became
    // ineligible (`outside text`). Retry must report #100, abort tasks.json and
    // input, and prevent Pass::sequence from promoting #100.
    {
        let (b, mut parent, _) = parent_fixture();
        parent["labels"] = json!([{"name":"tracker"}]);
        parent["body"] = json!("Updated scope");
        fs::write(b.path("repos_sam_box_issues_7"), parent.to_string()).unwrap();
        let child_100 = json!({
            "number": 100,
            "state": "open",
            "user": burrow_github::fake::actor("workers[bot]"),
            "labels": [{"name": "later"}],
            "title": "already created child",
            "body": "Body for already created child"
        });
        fs::write(b.path("repos_sam_box_issues_100"), child_100.to_string()).unwrap();
        fs::write(
            b.path("repos_sam_box_issues_99"),
            issue(99, "sam", &["outside text"]).to_string(),
        )
        .unwrap();

        let tasks = vec![
            task("already created child", None),
            task("reused issue", Some(99)),
        ];
        let input = b.pass().issue_text("sam/box", &parent, &[]);
        assert!(b.pass().prepare_plan("sam/box", "7", &input, &tasks));
        let path = b.pass().plan_dir("sam/box", "7").join("tasks.json");
        let mut list: List = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        list.children[0].attempted = true;
        list.children[0].posting_id = Some(burrow_github::fake::author("workers[bot]").id);
        list.children[0].number = Some(100);
        list.children[1].number = Some(99);
        assert!(b.pass().save_list("sam/box", "7", &list));

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
        let posted = b.posted();
        assert!(posted.contains("Invalid split:"), "{posted}");
        assert!(
            !posted.contains("No children created"),
            "planner post-binding retry must not claim no children were created:\n{posted}"
        );
        assert!(posted.contains("Retained partial child #100"), "{posted}");
        assert!(
            posted.contains("no further children will be created; needs replanning."),
            "{posted}"
        );
        assert!(b.log().contains("labels[]=later"));
        assert!(!b.log().contains("labels[]=tracker"));
        assert!(
            !b.pass().has_task_list(Some("sam/box"), "7"),
            "post-binding invalid split must empty tasks.json"
        );
        assert!(
            !b.pass().plan_dir("sam/box", "7").join("input").exists(),
            "post-binding invalid split must remove input approval"
        );
        let after: List = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert!(after.tasks.is_empty());
        assert_eq!(after.source, None);
        assert!(after.retained.iter().any(|r| r.child.number == Some(100)));

        // Subsequent sequence pass must not promote #100 to ready.
        let mut seq = b.pass();
        seq.sequence("sam/box", std::slice::from_ref(&parent));
        assert!(
            !b.log().contains("repos/sam/box/issues/100/labels"),
            "sequence must not promote rejected split child #100:\n{}",
            b.log()
        );
        assert_eq!(
            seq.plan_hold("sam/box", "100"),
            Some(Place::Superseded("sam/box#7".into()))
        );
    }

    // 2. Worker split: pass 1 created #100 and bound reused #99, then hit a
    // transient error in a post-binding step. Before pass 2, #99 was deleted
    // (404). Retry must report #100, abort tasks.json and input, and prevent
    // Pass::sequence from promoting #100.
    {
        let (mut b, mut parent, _) = parent_fixture();
        b.cfg.scratch = b.path("scratch");
        b.job_ending("j1", "done", Some("split"), "Splitting work");
        let job = crate::job::Job::new(&b.cfg, "j1");
        let record = b.record("j1");
        parent["labels"] = json!([{"name":"claude"}]);
        fs::write(b.path("repos_sam_box_issues_7"), parent.to_string()).unwrap();
        let child_100 = json!({
            "number": 100,
            "state": "open",
            "user": burrow_github::fake::actor("workers[bot]"),
            "labels": [{"name": "later"}],
            "title": "already created child",
            "body": "Body for already created child"
        });
        fs::write(b.path("repos_sam_box_issues_100"), child_100.to_string()).unwrap();
        fs::write(b.path("gone"), "repos/sam/box/issues/99\n").unwrap();
        let input = b.pass().issue_text("sam/box", &parent, &[]);
        Pass::hand(&record, &input).unwrap();

        let tasks = vec![
            task("already created child", None),
            task("reused issue", Some(99)),
        ];
        fs::write(
            job.turn("1", "split"),
            serde_json::to_string(&tasks).unwrap(),
        )
        .unwrap();
        assert!(b.pass().prepare_plan("sam/box", "7", &input, &tasks));
        let path = b.pass().plan_dir("sam/box", "7").join("tasks.json");
        let mut list: List = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        list.children[0].attempted = true;
        list.children[0].posting_id = Some(burrow_github::fake::author("workers[bot]").id);
        list.children[0].number = Some(100);
        list.children[1].number = Some(99);
        assert!(b.pass().save_list("sam/box", "7", &list));

        let mut p = b.pass();
        p.jobs("sam/box", "box", std::slice::from_ref(&parent))
            .unwrap();
        assert!(p.unreached.is_empty(), "{:?}", p.unreached);
        assert!(!record.exists());
        let posted = b.posted();
        assert!(posted.contains("Invalid split:"), "{posted}");
        assert!(
            !posted.contains("No children created"),
            "worker post-binding retry must not claim no children were created:\n{posted}"
        );
        assert!(posted.contains("Retained partial child #100"), "{posted}");
        assert!(
            posted.contains("no further children will be created; needs replanning."),
            "{posted}"
        );
        assert!(b.log().contains("labels[]=later"));
        assert!(
            !b.pass().has_task_list(Some("sam/box"), "7"),
            "worker post-binding invalid split must empty tasks.json"
        );
        assert!(
            !b.pass().plan_dir("sam/box", "7").join("input").exists(),
            "worker post-binding invalid split must remove input approval"
        );
        let after: List = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert!(after.tasks.is_empty());
        assert_eq!(after.source, None);
        assert!(after.retained.iter().any(|r| r.child.number == Some(100)));

        // Subsequent sequence pass must not promote #100 to ready.
        let mut seq = b.pass();
        seq.sequence("sam/box", std::slice::from_ref(&parent));
        assert!(
            !b.log().contains("repos/sam/box/issues/100/labels"),
            "sequence must not promote rejected split child #100:\n{}",
            b.log()
        );
        assert_eq!(
            seq.plan_hold("sam/box", "100"),
            Some(Place::Superseded("sam/box#7".into()))
        );
    }

    // 3. An unrelated completed/bound older plan on a different `input` (even
    // if the rejected split happens to propose the same tasks) must not be
    // reported or cleared.
    {
        let (b, mut parent, _) = parent_fixture();
        let older_input = b.pass().issue_text("sam/box", &parent, &[]);
        let tasks = vec![
            task("already created child", None),
            task("reused issue", Some(99)),
        ];
        assert!(b.pass().prepare_plan("sam/box", "7", &older_input, &tasks));
        let d = b.pass().plan_dir("sam/box", "7");
        let path = d.join("tasks.json");
        let mut list: List = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        list.children[0].attempted = true;
        list.children[0].number = Some(100);
        list.children[1].number = Some(99);
        assert!(b.pass().save_list("sam/box", "7", &list));
        let before_tasks = fs::read(&path).unwrap();

        parent["body"] = json!("Different updated scope");
        let newer_input = b.pass().issue_text("sam/box", &parent, &[]);
        assert_ne!(older_input, newer_input);

        assert_eq!(
            b.pass()
                .incomplete_split_partial("sam/box", "7", &newer_input, &tasks),
            RetainedPartial::default()
        );
        assert!(
            b.pass()
                .abort_incomplete_split("sam/box", "7", &newer_input, &tasks)
        );
        assert_eq!(
            fs::read(&path).unwrap(),
            before_tasks,
            "unrelated older bound plan on a different input must remain intact"
        );
        assert!(
            d.join("input").exists(),
            "unrelated older plan input approval must remain intact"
        );
    }
}
