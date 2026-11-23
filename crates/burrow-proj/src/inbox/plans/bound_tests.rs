//! FieldmouseWorks/burrow#1457: an author's status note on a split parent
//! (FieldmouseWorks/Conary#504's) paused its list, and no replan could
//! start while a child's job named it, which `proj why` didn't say.

use std::fs;

use serde_json::{Value, json};

use crate::inbox::plans::landed_tests::{advanced, child, sequence};
use crate::inbox::plans::tests::{live_job, parent_fixture};
use crate::inbox::record::Wait;

/// Comments on #7 by sam, oldest first, as GitHub gives them.
fn comments_on_parent(b: &crate::inbox::sorting_tests::Box, bodies: &[&str]) {
    let comments: Vec<Value> = bodies
        .iter()
        .zip(5..)
        .map(|(body, id)| {
            json!({"id": id, "user": burrow_github::fake::actor("sam"), "body": body,
                "created_at": "2026-11-13T17:22:33Z"})
        })
        .collect();
    fs::write(
        b.path("repos_sam_box_issues_7_comments"),
        json!([comments]).to_string(),
    )
    .unwrap();
}

#[test]
fn a_note_after_the_split_neither_pauses_its_list_nor_has_it_replanned() {
    let (b, parent, _) = parent_fixture();
    child(&b, "open");
    comments_on_parent(
        &b,
        &["Note: #8 accepted.", "note: focus undo list, re-paused"],
    );
    assert_eq!(b.pass().same_source("sam/box", &parent), Some(true));
    sequence(&b, &parent);
    assert!(!b.pass().paused("sam/box", "7"));
}

#[test]
fn an_unmarked_comment_after_the_split_still_pauses_its_list() {
    for bodies in [
        &["Drop task 1."][..],
        &["Note: #8 accepted.", "Drop task 1."],
        &["Drop task 1.", "Note: #8 accepted."],
        &["Drop task 1. Note: it's done."],
    ] {
        let (b, parent, _) = parent_fixture();
        child(&b, "open");
        comments_on_parent(&b, bodies);
        assert_eq!(
            b.pass().same_source("sam/box", &parent),
            Some(false),
            "{bodies:?}"
        );
        sequence(&b, &parent);
        assert!(b.pass().paused("sam/box", "7"), "{bodies:?}");
        assert!(!advanced(&b));
    }
}

#[test]
fn a_list_replanned_with_a_comment_takes_notes_after_it_and_no_edit_to_it() {
    let (b, parent, _) = parent_fixture();
    let scope = json!({"id": 5, "user": burrow_github::fake::actor("sam"),
        "body": "Also cover the docs.", "created_at": "2026-11-13T17:22:33Z"});
    let input = b
        .pass()
        .issue_text("sam/box", &parent, std::slice::from_ref(&scope));
    b.pass().keep_planned("sam/box", "7", &input).unwrap();
    comments_on_parent(&b, &["Also cover the docs.", "Note: #8 accepted."]);
    assert_eq!(b.pass().same_source("sam/box", &parent), Some(true));
    // The comment it was planned with, edited (here, as a note), is a
    // change to what was planned.
    comments_on_parent(&b, &["Note: also cover the docs.", "Note: #8 accepted."]);
    assert_eq!(b.pass().same_source("sam/box", &parent), Some(false));
    // And gone, too.
    comments_on_parent(&b, &[]);
    assert_eq!(b.pass().same_source("sam/box", &parent), Some(false));
}

#[test]
fn why_names_the_live_job_that_keeps_a_paused_parent_from_its_replan() {
    let (b, parent, _) = parent_fixture();
    child(&b, "open");
    comments_on_parent(&b, &["Drop task 1."]);
    sequence(&b, &parent);
    assert!(b.pass().paused("sam/box", "7"));
    let why = b.pass().look().record("sam/box", "7", Some(&parent));
    assert!(matches!(why.wait, Some(Wait::Stuck(_))), "{why:?}");
    // #8's job is live, and its brief names #7.
    live_job(&b, "running", "8");
    let why = b.pass().look().record("sam/box", "7", Some(&parent));
    assert_eq!(why.wait, Some(Wait::Job("lane".into())), "{why:?}");
    assert!(why.next.contains("lane, whose brief names it"), "{why:?}");
    let held = b.pass().place("sam/box", "9");
    assert!(
        matches!(&held, Some(super::super::Place::Stuck(w)) if w.contains("once lane, whose brief names it, is done")),
        "{held:?}"
    );
}
