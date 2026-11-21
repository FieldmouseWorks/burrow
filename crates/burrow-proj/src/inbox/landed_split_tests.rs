//! A child split again is done when its own list is, read from its
//! children as GitHub has them (landed.rs, `split_landed`).
use super::super::sorting_tests::{Box, issue};
use super::landed_tests::{advanced, child, sequence, stored, stored_at};
use super::tests::parent_fixture;
use super::*;

/// #7's child #8 split again into `children`, those given a PR already
/// ticked with it.
fn split(b: &Box, children: &[(u64, Option<&str>)]) {
    let p = b.pass();
    let mut list = List {
        source: None,
        revision: 0,
        retained: vec![],
        tasks: (0..children.len())
            .map(|i| Task {
                issue: None,
                carries: None,
                title: format!("sub-task {i}"),
                body: "one PR".into(),
                check: "regression".into(),
            })
            .collect(),
        children: children
            .iter()
            .map(|(n, pr)| Child {
                number: Some(*n),
                offered: true,
                merged_pr: pr.map(str::to_owned),
                ..Child::default()
            })
            .collect(),
        comment: Some(101),
        rendered: String::new(),
    };
    list.rendered = list.checklist("sam/box", "8");
    fs::create_dir_all(p.plan_dir("sam/box", "8")).unwrap();
    assert!(p.save_list("sam/box", "8", &list));
}

/// Issue N as GitHub has it now: `state`, and closed by `pr` if one.
fn issue_now(b: &Box, n: u64, state: &str, pr: Option<&str>) {
    let mut it = issue(n, "sam", &[]);
    it["state"] = json!(state);
    if state == "closed" {
        it["closed_at"] = json!("2026-11-21T06:52:00Z");
    }
    fs::write(b.path(&format!("repos_sam_box_issues_{n}")), it.to_string()).unwrap();
    if let Some(pr) = pr {
        fs::write(
            b.path(&format!("merged-{n}")),
            json!({"data":{"repository":{"issue":{"closedByPullRequestsReferences":{"nodes":[
                {"merged":true,"url":pr}]}}}}})
            .to_string(),
        )
        .unwrap();
    }
}

/// FieldmouseWorks/Conary#416: #7's child #8 was split again into #10 and
/// #11, and one PR closed #11 and #8 together, before any pass ticked #11
/// on #8's list. #8's list no longer moves on its own once #8 is closed,
/// so #7's must read it from #11 as GitHub has it: #8 is done, its list
/// says so, and #7 moves on without anyone running `proj plan done`.
#[test]
fn a_pr_closing_a_split_child_with_its_last_child_moves_the_grandparent_on() {
    let (b, parent, _) = parent_fixture();
    split(
        &b,
        &[
            (10, Some("https://github.com/sam/box/pull/500")),
            (11, None),
        ],
    );
    let pr = "https://github.com/sam/box/pull/519";
    issue_now(&b, 8, "closed", Some(pr));
    issue_now(&b, 11, "closed", Some(pr));
    sequence(&b, &parent);
    assert!(advanced(&b), "#7's list moves on to #9");
    let own = stored_at(&b, "8").checklist("sam/box", "8");
    assert!(
        own.contains(&format!("[x] #11: sub-task 1 — {pr}")),
        "{own}"
    );
    let top = stored(&b).checklist("sam/box", "7");
    assert!(top.contains("[x] #8: task 0"), "{top}");
}

/// The same split child closed while its last child is still open: its
/// list isn't done, so neither is it.
#[test]
fn a_split_child_closed_with_a_child_still_open_isnt_done() {
    let (b, parent, _) = parent_fixture();
    split(&b, &[(11, None)]);
    child(&b, "closed");
    issue_now(&b, 11, "open", None);
    sequence(&b, &parent);
    assert!(!advanced(&b));
    assert!(stored_at(&b, "8").children[0].merged_pr.is_none());
}

/// A split child still open is its own list's to move on: asking whether
/// it's done reads its children but writes nothing to its list.
#[test]
fn an_open_split_childs_list_is_read_not_written() {
    let (b, _, _) = parent_fixture();
    split(&b, &[(11, None)]);
    issue_now(
        &b,
        11,
        "closed",
        Some("https://github.com/sam/box/pull/519"),
    );
    assert_eq!(
        b.pass().landed_child("sam/box", 8, None),
        Some(Some("https://github.com/sam/box/issues/8".into()))
    );
    assert!(stored_at(&b, "8").children[0].merged_pr.is_none());
}

/// A sub-child whose comment says it continued as the grandparent: the
/// grandparent's list is already being read above it, so the walk ends
/// there, not done, rather than reading it again for ever.
#[test]
fn a_sub_child_continued_as_its_grandparent_ends_the_walk() {
    let (b, parent, _) = parent_fixture();
    split(&b, &[(11, None)]);
    child(&b, "closed");
    issue_now(&b, 11, "closed", None);
    let c = json!({"id": 5, "user": burrow_github::fake::actor("sam"),
        "body": "Continued as #7", "created_at": "2026-11-13T17:22:33Z"});
    fs::write(
        b.path("repos_sam_box_issues_11_comments"),
        json!([[c]]).to_string(),
    )
    .unwrap();
    sequence(&b, &parent);
    assert!(!advanced(&b));
    assert!(stored_at(&b, "8").children[0].merged_pr.is_none());
}
