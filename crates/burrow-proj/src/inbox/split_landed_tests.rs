//! A child split again is done when its own list is, read from that list's
//! children as GitHub has them now (landed.rs): its list stops being
//! sequenced once it closes, so a PR that closes it with its last child
//! must still move its parent's list on (FieldmouseWorks/burrow#1357).
use super::super::sorting_tests::{Box, issue};
use super::tests::parent_fixture;
use super::*;

const PR: &str = "https://github.com/sam/box/pull/519";

/// #7's list has #8 then #9; #8 was split again into #10, which landed,
/// then #11. GitHub has #8 closed and #11 `state`.
fn split(state: &str) -> (Box, Value) {
    let (b, parent, _) = parent_fixture();
    let p = b.pass();
    let mut list = List {
        source: None,
        revision: 0,
        retained: vec![],
        tasks: (10..12)
            .map(|i| Task {
                issue: None,
                carries: None,
                title: format!("task {i}"),
                body: "one PR".into(),
                check: "regression".into(),
            })
            .collect(),
        children: vec![
            Child {
                number: Some(10),
                merged_pr: Some("https://github.com/sam/box/pull/500".into()),
                ..Child::default()
            },
            Child {
                number: Some(11),
                ..Child::default()
            },
        ],
        comment: Some(101),
        rendered: String::new(),
    };
    list.rendered = list.checklist("sam/box", "8");
    fs::create_dir_all(p.plan_dir("sam/box", "8")).unwrap();
    assert!(p.save_list("sam/box", "8", &list));
    for (n, state) in [(8, "closed"), (11, state)] {
        let mut it = issue(n, "sam", &[]);
        it["state"] = json!(state);
        it["closed_at"] = json!("2026-11-21T06:52:00Z");
        fs::write(b.path(&format!("repos_sam_box_issues_{n}")), it.to_string()).unwrap();
        fs::write(
            b.path(&format!("repos_sam_box_issues_{n}_comments")),
            "[[]]",
        )
        .unwrap();
    }
    let eight: Value =
        serde_json::from_slice(&fs::read(b.path("repos_sam_box_issues_8")).unwrap()).unwrap();
    let source = p.issue_text("sam/box", &eight, &[]);
    p.keep_planned("sam/box", "8", &source).unwrap();
    (b, parent)
}

/// A merged PR closes `n`.
fn closes(b: &Box, n: u64) {
    fs::write(
        b.path(&format!("merged-{n}")),
        json!({"data":{"repository":{"issue":{"closedByPullRequestsReferences":{"nodes":[
            {"merged":true,"url":PR}]}}}}})
        .to_string(),
    )
    .unwrap();
}

fn list_of(b: &Box, n: &str) -> List {
    serde_json::from_slice(&fs::read(b.pass().plan_dir("sam/box", n).join("tasks.json")).unwrap())
        .unwrap()
}

fn moved_on(b: &Box) -> bool {
    b.pass().child_admissible("sam/box", "9")
}

/// FieldmouseWorks/Conary#519 said `Fixes #452. Fixes #416`: #416 (here
/// #8) closed with its last child before any pass ticked it. #8's list is
/// done by its children's own state, kept so, and #7's list moves on with
/// no `proj plan done`.
#[test]
fn a_pr_closing_a_split_child_with_its_last_child_moves_the_grandparent_on() {
    let (b, parent) = split("closed");
    closes(&b, 11);
    closes(&b, 8);
    let mut p = b.pass();
    p.sequence("sam/box", std::slice::from_ref(&parent));
    assert!(moved_on(&b), "{}", b.log());
    let eight = list_of(&b, "8");
    assert_eq!(eight.children[1].merged_pr.as_deref(), Some(PR));
    assert!(eight.rendered.contains(&format!("[x] #11: task 11 — {PR}")));
    let seven = list_of(&b, "7");
    assert!(seven.children[0].merged_pr.is_some());
    assert!(!seven.children[0].waits);
}

/// Done only when its own list is: a PR closing the split child itself
/// doesn't stand in for a last child closed with no PR, nor for one
/// reopened since an old PR closed it.
#[test]
fn a_split_child_whose_last_child_hasnt_landed_holds_the_grandparent() {
    for state in ["closed", "open"] {
        let (b, parent) = split(state);
        closes(&b, 8);
        if state == "open" {
            closes(&b, 11);
        }
        b.pass().sequence("sam/box", std::slice::from_ref(&parent));
        assert!(!moved_on(&b), "{state}");
        assert_eq!(list_of(&b, "8").children[1].merged_pr, None, "{state}");
        assert!(list_of(&b, "7").children[0].waits, "{state}");
    }
}

/// A last child GitHub couldn't be asked about is a miss, never a list
/// that waits on a child closed by hand.
#[test]
fn an_unreadable_last_child_is_a_miss_not_a_wait() {
    let (b, parent) = split("closed");
    closes(&b, 8);
    fs::write(b.path("fail"), "api repos/sam/box/issues/11\n").unwrap();
    let mut p = b.pass();
    p.sequence("sam/box", std::slice::from_ref(&parent));
    assert!(!moved_on(&b));
    assert!(p.unreached.contains(&"sam/box".to_owned()), "{}", b.log());
    assert!(!list_of(&b, "7").children[0].waits);
}

/// An open last child an author recorded a landing for is closed with it,
/// as its list's own pass would close it, and counts.
#[test]
fn an_open_last_child_recorded_landed_is_closed_and_counts() {
    let (b, parent) = split("open");
    let d = b.pass().plan_dir("sam/box", "11");
    fs::create_dir_all(&d).unwrap();
    fs::write(d.join("landed"), format!("{PR}\n")).unwrap();
    b.pass().sequence("sam/box", std::slice::from_ref(&parent));
    assert!(
        b.log()
            .contains("-X PATCH repos/sam/box/issues/11 -f state=closed"),
        "{}",
        b.log()
    );
    assert!(moved_on(&b));
    assert_eq!(list_of(&b, "8").children[1].merged_pr.as_deref(), Some(PR));
}

/// A split list that names its own split child is a cycle: not done, and
/// the pass returns.
#[test]
fn a_split_list_naming_itself_is_not_done() {
    let (b, parent) = split("closed");
    closes(&b, 8);
    let mut eight = list_of(&b, "8");
    eight.children[1].number = Some(8);
    assert!(b.pass().save_list("sam/box", "8", &eight));
    b.pass().sequence("sam/box", std::slice::from_ref(&parent));
    assert!(!moved_on(&b));
    assert!(list_of(&b, "7").children[0].waits);
}

/// A split child changed after its split is replanned, not walked: its
/// list's pass pauses it, and so does its parent's.
#[test]
fn a_split_child_changed_after_its_split_holds_the_grandparent() {
    let (b, parent) = split("closed");
    closes(&b, 11);
    let mut eight: Value =
        serde_json::from_slice(&fs::read(b.path("repos_sam_box_issues_8")).unwrap()).unwrap();
    eight["body"] = json!("A wider scope.");
    fs::write(b.path("repos_sam_box_issues_8"), eight.to_string()).unwrap();
    b.pass().sequence("sam/box", std::slice::from_ref(&parent));
    assert!(!moved_on(&b));
    assert_eq!(list_of(&b, "8").children[1].merged_pr, None);
}
