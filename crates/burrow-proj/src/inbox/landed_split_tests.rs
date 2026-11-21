//! A child split again is done when its own list is, read from its
//! children as GitHub has them (landed.rs, `split_landed`).
use super::super::sorting_tests::{Box, issue};
use super::landed_tests::{advanced, child, sequence, stored, stored_at};
use super::tests::{live_job, parent_fixture};
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
    // #8 as it was split: a change to it since holds its list.
    fs::write(b.path("repos_sam_box_issues_8_comments"), "[[]]").unwrap();
    let source = p.issue_text("sam/box", &issue(8, "sam", &[]), &[]);
    p.keep_planned("sam/box", "8", &source).unwrap();
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
    issue_now(&b, 8, "open", None);
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

/// The closed split child's ticks are kept but its checklist's update
/// fails: nothing else renders a closed list, so #7 holds until a pass
/// gets it through, then moves on.
#[test]
fn a_closed_split_childs_checklist_that_didnt_render_holds_until_it_does() {
    let (b, parent, _) = parent_fixture();
    split(&b, &[(11, None)]);
    let pr = "https://github.com/sam/box/pull/519";
    issue_now(&b, 8, "closed", Some(pr));
    issue_now(&b, 11, "closed", Some(pr));
    fs::write(b.path("fail"), "issues/comments/101\n").unwrap();
    sequence(&b, &parent);
    assert!(!advanced(&b));
    let own = stored_at(&b, "8");
    assert!(own.children[0].merged_pr.is_some(), "the tick is kept");
    assert_ne!(own.rendered, own.checklist("sam/box", "8"));

    fs::remove_file(b.path("fail")).unwrap();
    sequence(&b, &parent);
    assert!(advanced(&b));
    let own = stored_at(&b, "8");
    assert_eq!(own.rendered, own.checklist("sam/box", "8"));
}

/// FieldmouseWorks/burrow#1363: #8's scope changed after its split, then a
/// PR closed it with its last child. Its own pass would hold its list for
/// replanning, so the walk does too: nothing on #8's list is ticked and
/// #7's doesn't move on past work that waits to be replanned.
#[test]
fn a_split_child_changed_after_its_split_holds_the_grandparent() {
    let (b, parent, _) = parent_fixture();
    split(&b, &[(11, None)]);
    let pr = "https://github.com/sam/box/pull/519";
    issue_now(&b, 8, "closed", Some(pr));
    issue_now(&b, 11, "closed", Some(pr));
    let mut eight: Value =
        serde_json::from_slice(&fs::read(b.path("repos_sam_box_issues_8")).unwrap()).unwrap();
    eight["body"] = json!("A wider scope.");
    fs::write(b.path("repos_sam_box_issues_8"), eight.to_string()).unwrap();
    let mut p = b.pass();
    p.sequence("sam/box", std::slice::from_ref(&parent));
    assert!(!advanced(&b), "{}", b.log());
    assert!(stored_at(&b, "8").children[0].merged_pr.is_none());
    // A wait said to the owner, not a miss looked at again for ever.
    assert!(stored(&b).children[0].waits);
    assert!(!p.unreached.contains(&"sam/box".to_owned()), "{}", b.log());
}

/// While #8's own job is live its list is that job's, as its own pass
/// holds it: the walk reads nothing on it, and moves on once it ends.
#[test]
fn a_split_childs_own_live_job_holds_the_grandparent() {
    let (b, parent, _) = parent_fixture();
    split(&b, &[(11, None)]);
    let pr = "https://github.com/sam/box/pull/519";
    issue_now(&b, 8, "closed", Some(pr));
    issue_now(&b, 11, "closed", Some(pr));
    live_job(&b, "running", "8");
    let mut p = b.pass();
    p.sequence("sam/box", std::slice::from_ref(&parent));
    assert!(!advanced(&b), "{}", b.log());
    assert!(stored_at(&b, "8").children[0].merged_pr.is_none());
    assert!(stored(&b).children[0].waits);
    assert!(!p.unreached.contains(&"sam/box".to_owned()), "{}", b.log());

    fs::remove_file(b.cfg.jobs.join("lane/1.slot")).unwrap();
    fs::remove_file(b.path("active")).unwrap();
    sequence(&b, &parent);
    assert!(advanced(&b), "{}", b.log());
}

/// #8 closed before a pass ticked its last child #11, still open with a
/// landing an author recorded (`proj plan done`). Its own pass would close
/// #11 and count it; nothing else will once #8 is closed, so the walk does,
/// and #7 moves on. While #8 is open, #11 is its own pass's to close: the
/// walk writes nothing, and #8 isn't done yet.
#[test]
fn an_open_last_child_recorded_landed_is_closed_and_counts() {
    let pr = "https://github.com/sam/box/pull/519";
    for state in ["open", "closed"] {
        let (b, parent, _) = parent_fixture();
        split(&b, &[(11, None)]);
        issue_now(&b, 8, state, None);
        issue_now(&b, 11, "open", None);
        let d = b.pass().plan_dir("sam/box", "11");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("landed"), format!("{pr}\n")).unwrap();
        if state == "open" {
            assert_eq!(b.pass().landed_child("sam/box", 8, None), Some(None));
            assert!(
                !b.log().contains("issues/11 -f state=closed"),
                "{}",
                b.log()
            );
            continue;
        }
        // A close that doesn't go through is a miss: nothing ticked, and
        // #7 neither waits nor moves on until it does.
        fs::write(b.path("fail"), "state=closed\n").unwrap();
        let mut p = b.pass();
        p.sequence("sam/box", std::slice::from_ref(&parent));
        assert!(p.unreached.contains(&"sam/box".to_owned()), "{}", b.log());
        assert!(stored_at(&b, "8").children[0].merged_pr.is_none());
        assert!(!stored(&b).children[0].waits);
        assert!(!advanced(&b));
        fs::remove_file(b.path("fail")).unwrap();
        sequence(&b, &parent);
        assert!(
            b.log()
                .contains("-X PATCH repos/sam/box/issues/11 -f state=closed"),
            "{}",
            b.log()
        );
        assert!(advanced(&b), "{}", b.log());
        assert_eq!(
            stored_at(&b, "8").children[0].merged_pr.as_deref(),
            Some(pr)
        );
    }
}
