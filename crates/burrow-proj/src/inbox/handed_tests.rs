//! Closing a finished job's issue only while the issue is what the job was
//! handed: an edit, or a record that doesn't say, hands the job the issue as
//! it stands instead. Against [`super::sorting_tests`]'s stand-ins.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::sorting_tests::{Box, issue};

/// A comment of the author's, old enough to be handed on.
fn said(id: u64, body: &str) -> Value {
    json!({"id": id, "user": {"login": "sam"}, "body": body,
           "created_at": "2026-11-01T00:00:00Z"})
}

/// A box whose job j1 for issue 7 finished (`proj done`), its answer posted
/// already, and can be given another turn; its record says it was handed
/// `had` of the issue and `seen` its comments up to `seen`. GitHub has
/// `now` for the issue and `comments` on it. The record.
fn finished(b: &mut Box, had: &Value, seen: u64, comments: &[Value], now: &Value) -> PathBuf {
    // A turn starts: no unit of the job's is loaded or has a job queued.
    b.cfg.tools.systemd_run = "true".into();
    b.cfg.tools.systemctl = "false".into();
    b.job("j1", "done", "Fixed in #12.");
    fs::write(b.cfg.jobs.join("j1/session"), "s1\n").unwrap();
    let d = b.record("j1");
    fs::write(d.join("answered"), "1\n").unwrap();
    fs::write(d.join("seen"), format!("{seen}\n")).unwrap();
    let handed: Vec<Value> = comments
        .iter()
        .filter(|c| c["id"].as_u64().is_some_and(|i| i <= seen))
        .cloned()
        .collect();
    let input = b.pass().issue_text("sam/box", had, &handed);
    fs::write(d.join("input"), input).unwrap();
    fs::write(b.path("comments"), json!([comments]).to_string()).unwrap();
    let mut on = now.clone();
    on["state"] = json!("open");
    fs::write(b.path("repos_sam_box_issues_7"), on.to_string()).unwrap();
    d
}

/// What the job was handed in its next turn, after the pass looked at its
/// issue as GitHub has it (`now`): the issue isn't closed, and the job gets
/// it whole, which its record now says it was handed.
fn handed_whole(b: &Box, d: &Path, now: &Value, comments: &[Value]) -> String {
    b.pass()
        .jobs("sam/box", "box", std::slice::from_ref(now))
        .unwrap();
    assert!(!b.log().contains("state=closed"), "{}", b.log());
    assert!(d.exists());
    let prompt = fs::read_to_string(b.cfg.jobs.join("j1/prompt.2")).unwrap_or_default();
    assert!(
        prompt.starts_with("The issue has changed since you were handed it"),
        "{prompt}"
    );
    let whole = b.pass().issue_text("sam/box", now, comments);
    assert!(prompt.contains(&whole), "{prompt}");
    assert_eq!(fs::read_to_string(d.join("input")).unwrap(), whole);
    // Handed it, and nothing more since: closed next time.
    fs::write(b.cfg.jobs.join("j1/turns"), "1\n").unwrap();
    b.pass()
        .jobs("sam/box", "box", std::slice::from_ref(now))
        .unwrap();
    assert!(b.log().contains("state=closed"), "{}", b.log());
    prompt
}

#[test]
fn a_body_edited_while_its_job_ran_is_handed_on() {
    let had = issue(7, "sam", &["claude"]);
    let mut now = had.clone();
    now["body"] = json!("It's broken, and the docs need it too.");
    let mut b = Box::new();
    let d = finished(&mut b, &had, 0, &[], &now);
    let prompt = handed_whole(&b, &d, &now, &[]);
    assert!(prompt.contains("the docs need it too"), "{prompt}");
}

#[test]
fn a_title_edited_while_its_job_ran_is_handed_on() {
    let had = issue(7, "sam", &["claude"]);
    let mut now = had.clone();
    now["title"] = json!("Fix it, and the docs");
    let mut b = Box::new();
    let d = finished(&mut b, &had, 0, &[], &now);
    let prompt = handed_whole(&b, &d, &now, &[]);
    assert!(prompt.contains("# Fix it, and the docs"), "{prompt}");
}

#[test]
fn a_comment_edited_while_its_job_ran_is_handed_on() {
    let now = issue(7, "sam", &["claude"]);
    let mut b = Box::new();
    let d = finished(&mut b, &now, 5, &[said(5, "the red one")], &now);
    // Edited after the job was handed it.
    let comments = [said(5, "the blue one, not the red")];
    fs::write(b.path("comments"), json!([comments]).to_string()).unwrap();
    let prompt = handed_whole(&b, &d, &now, &comments);
    assert!(prompt.contains("the blue one, not the red"), "{prompt}");
}

#[test]
fn a_comment_deleted_while_its_job_ran_is_handed_on() {
    let now = issue(7, "sam", &["claude"]);
    let mut b = Box::new();
    let d = finished(&mut b, &now, 5, &[said(5, "and the docs too")], &now);
    fs::write(b.path("comments"), "[[]]").unwrap();
    let prompt = handed_whole(&b, &d, &now, &[]);
    assert!(!prompt.contains("and the docs too"), "{prompt}");
}

// A record from before what each job was handed was kept can't say the
// issue is unchanged: its job is handed the issue as it stands.
#[test]
fn a_record_without_what_its_job_had_is_handed_on() {
    let now = issue(7, "sam", &["claude"]);
    let mut b = Box::new();
    let d = finished(&mut b, &now, 5, &[said(5, "the red one")], &now);
    fs::remove_file(d.join("input")).unwrap();
    handed_whole(&b, &d, &now, &[said(5, "the red one")]);
}

// Unchanged, a reply goes on alone, as before, and what the job had then
// includes it: no edit, so the issue closes after.
#[test]
fn a_reply_alone_is_handed_as_a_reply() {
    let now = issue(7, "sam", &["claude"]);
    let mut b = Box::new();
    let comments = [said(5, "the red one"), said(6, "and the blue")];
    let d = finished(&mut b, &now, 5, &comments, &now);
    b.pass()
        .jobs("sam/box", "box", std::slice::from_ref(&now))
        .unwrap();
    let prompt = fs::read_to_string(b.cfg.jobs.join("j1/prompt.2")).unwrap();
    assert_eq!(prompt, "sam replied on the issue:\n\nand the blue\n");
    assert_eq!(
        fs::read_to_string(d.join("input")).unwrap(),
        b.pass().issue_text("sam/box", &now, &comments)
    );
    fs::write(b.cfg.jobs.join("j1/turns"), "1\n").unwrap();
    b.pass()
        .jobs("sam/box", "box", std::slice::from_ref(&now))
        .unwrap();
    assert!(b.log().contains("state=closed"), "{}", b.log());
}
