//! A closed child whose record was the deliverable, done by an author's
//! `proj plan done OWNER/REPO#N --no-pr REASON` (plan_done.rs), bound to
//! the closure it was recorded on (FieldmouseWorks/Conary#548).
use super::super::sorting_tests::{Box, issue};
use super::landed_tests::{advanced, notice_open, notifier, sequence, stored};
use super::tests::parent_fixture;
use super::*;

const REASON: &str = "the per-repo tables on the parent are the record";

/// #8 as GitHub has it now: `state`, closed `at` when closed.
fn child_at(b: &Box, state: &str, at: Option<&str>) {
    let mut child = issue(8, "sam", &["later"]);
    child["state"] = json!(state);
    child["closed_at"] = json!(at);
    fs::write(b.path("repos_sam_box_issues_8"), child.to_string()).unwrap();
}

fn author() -> Option<u64> {
    Some(burrow_github::fake::author("sam").id)
}

fn kept(b: &Box) -> bool {
    b.pass().plan_dir("sam/box", "8").join("no-pr").exists()
}

#[test]
fn a_closed_child_an_author_marks_done_without_a_pr_moves_the_list_on() {
    let (mut b, parent, _) = parent_fixture();
    notifier(&mut b);
    child_at(&b, "closed", Some("2026-11-22T10:00:00Z"));
    sequence(&b, &parent);
    assert!(!advanced(&b));
    assert!(notice_open(&b));
    assert_eq!(
        b.pass()
            .plan_done_without_pr("Sam/Box#8", REASON, author())
            .unwrap(),
        "sam/box"
    );
    sequence(&b, &parent);
    assert!(advanced(&b), "the next child is admitted");
    let checklist = stored(&b).checklist("sam/box", "7");
    assert!(
        checklist.contains(&format!("- [x] #8: task 0 — no PR needed: {REASON}")),
        "{checklist}"
    );
    assert!(!checklist.contains("the list waits"), "{checklist}");
    assert!(!notice_open(&b));
    assert!(!b.log().contains("issues/8 -f state=closed"));
}

#[test]
fn only_an_author_marks_a_closed_child_of_a_managed_list() {
    let (b, parent, _) = parent_fixture();
    child_at(&b, "closed", Some("2026-11-22T10:00:00Z"));
    let stranger = Some(burrow_github::fake::author("stranger").id);
    for (child, caller) in [
        ("sam/box#8", stranger),
        ("sam/box#8", None),
        ("sam/else#8", author()),
        ("sam/box#5", author()),
        ("sam/box", author()),
    ] {
        assert!(
            b.pass()
                .plan_done_without_pr(child, REASON, caller)
                .is_err(),
            "{child} {caller:?}"
        );
        sequence(&b, &parent);
        assert!(!advanced(&b), "{child} {caller:?}");
        assert!(!kept(&b), "{child} {caller:?}");
    }
    // An open child isn't marked: its own work may still go on.
    child_at(&b, "open", None);
    assert!(
        b.pass()
            .plan_done_without_pr("sam/box#8", REASON, author())
            .is_err()
    );
    assert!(!kept(&b));
    sequence(&b, &parent);
    assert!(!advanced(&b));
}

/// Reopened before a pass read it, the child's work goes on: it isn't
/// closed again, and reclosed it waits on the list as any hand close does.
#[test]
fn a_reopened_or_reclosed_child_drops_what_was_marked() {
    let (b, parent, _) = parent_fixture();
    child_at(&b, "closed", Some("2026-11-22T10:00:00Z"));
    b.pass()
        .plan_done_without_pr("sam/box#8", REASON, author())
        .unwrap();
    child_at(&b, "open", None);
    sequence(&b, &parent);
    assert!(!advanced(&b));
    assert!(!b.log().contains("issues/8 -f state=closed"), "{}", b.log());
    child_at(&b, "closed", Some("2026-11-23T09:00:00Z"));
    sequence(&b, &parent);
    assert!(!advanced(&b));
    assert!(stored(&b).children[0].waits);
}
