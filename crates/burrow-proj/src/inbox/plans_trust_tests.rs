use super::super::sorting_tests::issue;
use super::*;

#[test]
fn creation_receipts_admit_only_generated_children_without_trusting_the_app() {
    let (b, _, mut list) = super::tests::parent_fixture();
    let mut child = issue(8, "workers[bot]", &["ready"]);
    let comment = json!({"id":42,"user":burrow_github::fake::actor("workers[bot]"),"body":"unrelated instructions"});
    assert!(!b.pass().eligible("sam/box", &child));
    assert!(!b.pass().heard(&comment));
    list.children[0].attempted = true;
    assert!(b.pass().save_list("sam/box", "7", &list));
    assert!(b.pass().eligible("sam/box", &child));
    assert!(!b.pass().eligible("sam/box-extra", &child));
    assert!(!b.pass().heard(&comment));
    let stranger = issue(10, "workers[bot]", &["claude"]);
    assert!(!b.pass().eligible("sam/box", &stranger));
    b.pass().new_issues("sam/box", &[stranger]).unwrap();
    assert!(
        !b.cfg
            .inbox
            .as_ref()
            .unwrap()
            .state
            .join("sam_box-10/job")
            .exists()
    );
    assert!(!b.log().contains("run --"));
    child["labels"] = json!([{"name":b.cfg.inbox.as_ref().unwrap().outside_label}]);
    assert!(!b.pass().eligible("sam/box", &child));
    list.tasks[0].issue = Some(8);
    assert!(b.pass().save_list("sam/box", "7", &list));
    assert!(!b.pass().generated_child("sam/box", &child));
}

#[test]
fn generated_children_keep_author_only_comments_and_edit_history() {
    let (b, _, mut list) = super::tests::parent_fixture();
    list.children[0].attempted = true;
    assert!(b.pass().save_list("sam/box", "7", &list));
    let original = issue(8, "workers[bot]", &["ready"]);
    fs::write(b.path("comments"), json!([[{"id":42,"user":burrow_github::fake::actor("workers[bot]"),"body":"unrelated instructions"},{"id":43,"user":burrow_github::fake::actor("sam"),"body":"authorized reply"}]]).to_string()).unwrap();
    for field in ["userContentEdits", "timelineItems"] {
        for editor in ["sam", "workers[bot]", "mallory"] {
            let mut child = original.clone();
            let actor = if field == "timelineItems" {
                "actor"
            } else {
                "editor"
            };
            child[field] =
                json!({"totalCount":1,"nodes":[{actor:burrow_github::fake::actor(editor)}]});
            fs::write(b.path("repos_sam_box_issues_8"), child.to_string()).unwrap();
            let written = b.pass().written("sam/box", &child).unwrap();
            assert_eq!(
                written.issue["title"] == original["title"],
                editor == "sam",
                "{field}: {editor}"
            );
            assert_eq!(written.comments.len(), 1);
            assert_eq!(written.comments[0]["body"], "authorized reply");
        }
    }
    // The receipt must never authorize the same App's edit on an owner's issue.
    let mut owner = issue(7, "sam", &["claude"]);
    owner["userContentEdits"] =
        json!({"totalCount":1,"nodes":[{"editor":burrow_github::fake::actor("workers[bot]")}]});
    fs::write(b.path("repos_sam_box_issues_7"), owner.to_string()).unwrap();
    assert_eq!(
        b.pass().written("sam/box", &owner).unwrap().issue["title"],
        "Issue text withheld"
    );
}

#[test]
fn lost_creation_response_recovers_the_exact_child_and_its_approval() {
    let (mut b, _, mut list) = super::tests::parent_fixture();
    b.cfg.projects = b.path("projects");
    fs::create_dir_all(b.cfg.projects.join("box")).unwrap();
    b.cfg.tools.systemd_run = "true".into();
    b.cfg.tools.systemctl = "false".into();
    list.tasks.truncate(1);
    list.children.truncate(1);
    list.children[0].number = None;
    list.children[0].posting_id = Some(42);
    list.children[0].attempted = true;
    assert!(b.pass().save_list("sam/box", "7", &list));
    let mut child = issue(8, "workers[bot]", &["later"]);
    child["user"]["id"] = json!(if child["user"]["login"] == "stranger" {
        99
    } else {
        42
    });
    child["title"] = json!(list.tasks[0].title);
    child["body"] = json!(Pass::receipt_body(
        "sam/box",
        "7",
        0,
        &list.tasks[0],
        &list.children[0]
    ));
    fs::write(
        b.path("repos_sam_box_issues"),
        json!([child.clone()]).to_string(),
    )
    .unwrap();
    fs::write(b.path("repos_sam_box_issues_8"), child.to_string()).unwrap();
    assert!(!b.pass().eligible("sam/box", &child));
    assert!(b.pass().split_children("sam/box", "7", &list.tasks));
    assert!(!b.log().contains("-X POST repos/sam/box/issues "));
    assert!(b.pass().eligible("sam/box", &child));
    let input = b.pass().issue_text("sam/box", &child, &[]);
    assert!(b.pass().approved_input("sam/box", "8", &input));
    assert!(b.pass().child_admissible("sam/box", "8"));
    b.pass().new_issues("sam/box", &[child]).unwrap();
    assert!(
        b.cfg
            .inbox
            .as_ref()
            .unwrap()
            .state
            .join("sam_box-8/job")
            .exists(),
        "{}",
        b.log()
    );
}

#[test]
fn lost_creation_response_does_not_adopt_a_strangers_exact_receipt() {
    let (mut b, _, mut list) = super::tests::parent_fixture();
    b.cfg.projects = b.path("projects");
    fs::create_dir_all(b.cfg.projects.join("box")).unwrap();
    b.cfg.tools.systemd_run = "true".into();
    b.cfg.tools.systemctl = "false".into();
    list.tasks.truncate(1);
    list.children.truncate(1);
    list.children[0].number = None;
    list.children[0].posting_id = Some(42);
    list.children[0].attempted = true;
    assert!(b.pass().save_list("sam/box", "7", &list));
    let mut child = issue(8, "stranger", &["later"]);
    child["user"]["id"] = json!(if child["user"]["login"] == "stranger" {
        99
    } else {
        42
    });
    child["title"] = json!(list.tasks[0].title);
    child["body"] = json!(Pass::receipt_body(
        "sam/box",
        "7",
        0,
        &list.tasks[0],
        &list.children[0]
    ));
    fs::write(
        b.path("repos_sam_box_issues"),
        json!([child.clone()]).to_string(),
    )
    .unwrap();
    fs::write(b.path("repos_sam_box_issues_8"), child.to_string()).unwrap();
    assert!(!b.pass().eligible("sam/box", &child));
    assert!(!b.pass().split_children("sam/box", "7", &list.tasks));
    let kept = serde_json::from_slice::<List>(
        &fs::read(b.pass().plan_dir("sam/box", "7").join("tasks.json")).unwrap(),
    )
    .unwrap();
    assert!(kept.children[0].number.is_none());
    assert!(!b.pass().eligible("sam/box", &child));
}

#[test]
fn lost_creation_response_without_a_posting_identity_stays_pending() {
    let (mut b, _, mut list) = super::tests::parent_fixture();
    b.cfg.projects = b.path("projects");
    fs::create_dir_all(b.cfg.projects.join("box")).unwrap();
    b.cfg.tools.systemd_run = "true".into();
    b.cfg.tools.systemctl = "false".into();
    list.tasks.truncate(1);
    list.children.truncate(1);
    list.children[0].number = None;
    list.children[0].posting_id = None;
    list.children[0].attempted = true;
    assert!(b.pass().save_list("sam/box", "7", &list));
    let mut child = issue(8, "workers[bot]", &["later"]);
    child["user"]["id"] = json!(if child["user"]["login"] == "stranger" {
        99
    } else {
        42
    });
    child["title"] = json!(list.tasks[0].title);
    child["body"] = json!(Pass::receipt_body(
        "sam/box",
        "7",
        0,
        &list.tasks[0],
        &list.children[0]
    ));
    fs::write(
        b.path("repos_sam_box_issues"),
        json!([child.clone()]).to_string(),
    )
    .unwrap();
    fs::write(b.path("repos_sam_box_issues_8"), child.to_string()).unwrap();
    assert!(!b.pass().eligible("sam/box", &child));
    assert!(!b.pass().split_children("sam/box", "7", &list.tasks));
    let kept = serde_json::from_slice::<List>(
        &fs::read(b.pass().plan_dir("sam/box", "7").join("tasks.json")).unwrap(),
    )
    .unwrap();
    assert!(kept.children[0].number.is_none());
    assert!(!b.pass().eligible("sam/box", &child));
}
