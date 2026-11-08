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
    handed_whole_listed(b, d, now, now, comments)
}

/// [`handed_whole`], with the pass's list of issues having `listed` for it
/// (read before GitHub had `now`).
fn handed_whole_listed(
    b: &Box,
    d: &Path,
    listed: &Value,
    now: &Value,
    comments: &[Value],
) -> String {
    b.pass()
        .jobs("sam/box", "box", std::slice::from_ref(listed))
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

// Edited after the pass read its list of issues, while it saw to other jobs:
// GitHub's issue as it is now decides, not the list.
#[test]
fn an_edit_after_the_list_was_read_is_handed_on() {
    let had = issue(7, "sam", &["claude"]);
    let mut now = had.clone();
    now["body"] = json!("It's broken, and the docs need it too.");
    let mut b = Box::new();
    let d = finished(&mut b, &had, 0, &[], &now);
    let prompt = handed_whole_listed(&b, &d, &had, &now, &[]);
    assert!(prompt.contains("the docs need it too"), "{prompt}");
}

// A comment the job's first turn was handed while it was under a minute old
// stays in what it's handed whole, and in its record, so a pass after that,
// with nothing changed, closes the issue rather than handing it on again.
#[test]
fn a_young_comment_the_job_had_stays_in_the_issue_handed_whole() {
    let had = issue(7, "sam", &["claude"]);
    let mut now = had.clone();
    now["body"] = json!("It's broken, and the docs need it too.");
    let just = jiff::Timestamp::now()
        .strftime("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    let mut red = said(5, "the red one");
    red["created_at"] = json!(just);
    let mut b = Box::new();
    let d = finished(&mut b, &had, 5, std::slice::from_ref(&red), &now);
    let prompt = handed_whole(&b, &d, &now, std::slice::from_ref(&red));
    assert!(prompt.contains("the red one"), "{prompt}");
}

// The real continuation boundary, not just the writer predicate: a reply
// must never carry a collaborator's edits in the refreshed issue either.
#[test]
fn a_reply_cannot_smuggle_outside_edits_into_a_running_job() {
    for field in ["body", "title", "comment"] {
        let had = issue(7, "sam", &["claude"]);
        let mut now = had.clone();
        let bad = json!({"totalCount":1,"nodes":[{"editor":{"login":"mallory"},"actor":{"login":"mallory"}}]});
        let mut comments = vec![said(9, "authorized continuation")];
        match field {
            "body" => {
                now["body"] = json!("MALICIOUS INPUT");
                now["userContentEdits"] = bad;
            }
            "title" => {
                now["title"] = json!("MALICIOUS INPUT");
                now["timelineItems"] = bad;
            }
            _ => {
                let mut c = said(10, "MALICIOUS INPUT");
                c["userContentEdits"] = bad;
                comments.push(c);
            }
        }
        let mut b = Box::new();
        finished(&mut b, &had, 0, &comments, &now);
        b.pass().jobs("sam/box", "box", &[now]).unwrap();
        let prompt = fs::read_to_string(b.cfg.jobs.join("j1/prompt.2")).unwrap();
        assert!(
            prompt.contains("authorized continuation"),
            "{field}: {prompt}"
        );
        assert!(!prompt.contains("MALICIOUS INPUT"), "{field}: {prompt}");
    }
}

#[test]
fn a_final_refresh_edit_retries_verification_before_closing_or_continuing() {
    use std::os::unix::fs::PermissionsExt;
    for field in ["title", "body"] {
        for outsider in [false, true] {
            let had = issue(7, "sam", &["claude"]);
            let mut edited = had.clone();
            edited[field] = json!("NEW REQUEST AFTER SNAPSHOT");
            edited["state"] = json!("open");
            if outsider {
                let history = if field == "title" {
                    "timelineItems"
                } else {
                    "userContentEdits"
                };
                edited[history] = json!({"totalCount":1,"nodes":[{"actor":{"login":"mallory"},"editor":{"login":"mallory"}}]});
            }
            let mut b = Box::new();
            let d = finished(&mut b, &had, 0, &[], &had);
            let input = fs::read_to_string(d.join("input")).unwrap();
            fs::write(b.path("new-issue"), edited.to_string()).unwrap();
            fs::write(b.path("hook"), format!("#!/bin/sh\nif [ \"$1\" = 'api repos/sam/box/issues/7' ]; then cp '{}' '{}'; fi\n", b.path("new-issue").display(), b.path("repos_sam_box_issues_7").display())).unwrap();
            fs::set_permissions(b.path("hook"), fs::Permissions::from_mode(0o755)).unwrap();
            let mut p = b.pass();
            p.jobs("sam/box", "box", std::slice::from_ref(&had))
                .unwrap();
            assert!(!b.log().contains("state=closed"));
            assert!(!b.cfg.jobs.join("j1/prompt.2").exists());
            assert_eq!(fs::read_to_string(d.join("input")).unwrap(), input);
            assert!(p.wake.contains_key("sam/box"));
            b.pass().jobs("sam/box", "box", &[edited]).unwrap();
            let prompt = fs::read_to_string(b.cfg.jobs.join("j1/prompt.2")).unwrap();
            assert_eq!(prompt.contains("NEW REQUEST AFTER SNAPSHOT"), !outsider);
            assert!(!b.log().contains("state=closed"));
        }
    }
}

#[test]
fn retiring_an_issue_reports_unread_author_replies_and_retries_failed_notices() {
    for state in ["closed", "open"] {
        let now = issue(7, "sam", &["claude"]);
        let mut b = Box::new();
        let comments = [
            said(5, "already handed"),
            said(6, "one more run"),
            json!({"id": 7, "user": {"login": "stranger"}, "body": "outside"}),
            said(8, "answer <!-- proj-inbox -->"),
        ];
        let d = finished(&mut b, &now, 5, &comments, &now);
        let mut ended = now.clone();
        ended["state"] = json!(state);
        ended["labels"] = json!([]);
        fs::write(b.path("repos_sam_box_issues_7"), ended.to_string()).unwrap();
        // A running turn defers the notice and keeps the unread cursor.
        b.cfg.tools.systemctl = "true".into();
        b.pass().jobs("sam/box", "box", &[]).unwrap();
        assert!(d.exists());
        assert!(!b.posted().contains("unread author replies"));
        b.cfg.tools.systemctl = b.path("systemctl");
        fs::write(
            b.cfg.jobs.join("j1/await"),
            "https://github.com/sam/box/pull/1\n",
        )
        .unwrap();
        fs::write(b.path("fail"), "api repos/sam/box/issues/7").unwrap();
        b.pass().jobs("sam/box", "box", &[]).unwrap();
        assert!(!d.exists());
        let retired = b
            .cfg
            .inbox
            .as_ref()
            .unwrap()
            .state
            .join(".retired/sam_box/7/j1");
        assert!(retired.exists());
        assert!(!b.cfg.jobs.join("j1/await").exists());
        assert!(b.cfg.jobs.join("j1/1.stopped").exists());
        fs::write(b.path("fail"), "api graphql").unwrap();
        b.pass().retired("sam/box").unwrap();
        assert!(retired.exists());
        fs::write(b.path("fail"), "body=@-").unwrap();
        b.pass().jobs("sam/box", "box", &[]).unwrap();
        assert!(!d.exists());
        assert!(retired.exists());
        // Reopening starts a fresh conversation while the old notice is pending.
        b.cfg.projects = b.path("projects");
        fs::create_dir_all(b.cfg.projects.join("box")).unwrap();
        fs::write(b.path("repos_sam_box_issues_7"), now.to_string()).unwrap();
        b.cfg.tools.systemctl = "false".into();
        b.pass()
            .new_issues("sam/box", std::slice::from_ref(&now))
            .unwrap();
        let fresh = fs::read_to_string(d.join("job")).unwrap();
        assert_ne!(fresh.trim(), "j1");
        assert_eq!(
            fs::read_to_string(b.cfg.jobs.join(fresh.trim()).join("turns"))
                .unwrap()
                .trim(),
            "1"
        );
        assert!(!b.cfg.jobs.join(fresh.trim()).join("session").exists());
        fs::remove_file(b.path("fail")).unwrap();
        b.cfg.tools.systemctl = b.path("systemctl");
        b.pass().retired("sam/box").unwrap();
        assert!(!retired.exists());
        assert_eq!(fs::read_to_string(d.join("job")).unwrap(), fresh);
        let posted = b.posted();
        assert!(posted.contains("unread author replies"), "{posted}");
        assert!(posted.contains("#issuecomment-6"), "{posted}");
        for id in [5, 7, 8] {
            assert!(!posted.contains(&format!("#issuecomment-{id}")), "{posted}");
        }
        assert!(!b.cfg.jobs.join("j1/prompt.2").exists());
    }
}

#[test]
fn retiring_an_issue_without_unread_replies_needs_no_notice() {
    let now = issue(7, "sam", &["claude"]);
    let mut b = Box::new();
    let d = finished(&mut b, &now, 5, &[said(5, "already handed")], &now);
    b.cfg.tools.systemctl = b.path("systemctl");
    let mut ended = now.clone();
    ended["state"] = json!("closed");
    fs::write(b.path("repos_sam_box_issues_7"), ended.to_string()).unwrap();
    b.pass().jobs("sam/box", "box", &[]).unwrap();
    assert!(!d.exists());
    assert!(!b.posted().contains("unread author replies"));
}

#[test]
fn retirement_does_not_repeat_a_delivered_notice_when_stopping_failed() {
    let now = issue(7, "sam", &["claude"]);
    let mut b = Box::new();
    let d = finished(&mut b, &now, 0, &[said(6, "one more")], &now);
    // stop fails, but reporting can succeed independently.
    b.pass().jobs("sam/box", "box", &[]).unwrap();
    assert!(!d.exists());
    let retired = b
        .cfg
        .inbox
        .as_ref()
        .unwrap()
        .state
        .join(".retired/sam_box/7/j1");
    assert!(retired.join("reported").exists());
    let posted = b.posted();
    b.cfg.tools.systemctl = b.path("systemctl");
    b.pass().retired("sam/box").unwrap();
    assert!(!retired.exists());
    assert_eq!(b.posted(), posted);
}

#[test]
fn retirement_notices_resume_after_a_partial_delivery_with_bounded_bodies() {
    use std::os::unix::fs::PermissionsExt;
    let now = issue(7, "sam", &["claude"]);
    let mut b = Box::new();
    let comments: Vec<Value> = (1..=1400).map(|id| said(id, "one more")).collect();
    finished(&mut b, &now, 0, &comments, &now);
    b.cfg.tools.systemctl = b.path("systemctl");
    // Reject the second batch; the first must never be sent again.
    let hook = b.path("hook");
    fs::write(
        &hook,
        format!(
            r#"#!/bin/sh
case "$*" in *body=@-*)
    if [ -e '{first}' ]; then echo 'body=@-' >'{fail}'; else touch '{first}'; fi ;;
esac
"#,
            first = b.path("first").display(),
            fail = b.path("fail").display()
        ),
    )
    .unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    b.pass().jobs("sam/box", "box", &[]).unwrap();
    let first = b.posted();
    assert!(first.contains("#issuecomment-1\n"));
    assert!(!first.contains("#issuecomment-1400\n"));
    fs::remove_file(hook).unwrap();
    fs::remove_file(b.path("fail")).unwrap();
    b.pass().retired("sam/box").unwrap();
    let posted = b.posted();
    for id in 1..=1400 {
        assert_eq!(posted.matches(&format!("#issuecomment-{id}\n")).count(), 1);
    }
    // The fixture concatenates complete signed comments.
    for body in posted.split_inclusive("_proj-inbox on box_\n") {
        assert!(body.chars().count() <= 65536, "{}", body.chars().count());
    }
    assert!(
        !b.cfg
            .inbox
            .as_ref()
            .unwrap()
            .state
            .join(".retired/sam_box/7/j1")
            .exists()
    );
}

#[test]
fn long_repository_issue_and_job_names_retire_without_combining_components() {
    let mut b = Box::new();
    // Both existing record names fit NAME_MAX; their concatenation does not.
    let owner = "o".repeat(31);
    let project = "p".repeat(100);
    let repo = format!("{owner}/{project}");
    let key = super::key(&repo);
    let number = i64::MAX;
    let mut now = issue(number as u64, "sam", &["claude"]);
    now["state"] = json!("closed");
    fs::write(
        b.path(&format!("repos_{key}_issues_{number}")),
        now.to_string(),
    )
    .unwrap();
    // Keep successful reporting and failed stopping pending for two distinct
    // jobs of the same issue. Neither record may collide with the other.
    b.cfg.tools.systemctl = "false".into();
    let root = b.cfg.inbox.as_ref().unwrap().state.clone();
    for suffix in ["AAAA", "BBBB"] {
        let id = format!("{project}-20261108-092136-{suffix}");
        b.job(&id, "done", "finished");
        let active = root.join(format!("{key}-{number}"));
        fs::create_dir_all(&active).unwrap();
        for (file, body) in [
            ("job", id.clone()),
            ("issue", format!("{repo}#{number}")),
            ("answered", "1".into()),
            ("seen", "0".into()),
        ] {
            fs::write(active.join(file), body).unwrap();
        }
        b.pass().jobs(&repo, &project, &[]).unwrap();
        assert!(!active.exists());
        assert!(
            root.join(".retired")
                .join(&key)
                .join(number.to_string())
                .join(&id)
                .join("reported")
                .exists()
        );
    }
    let retired = root.join(".retired").join(&key).join(number.to_string());
    assert_eq!(fs::read_dir(&retired).unwrap().count(), 2);
    b.cfg.tools.systemctl = b.path("systemctl");
    b.pass().retired(&repo.to_uppercase()).unwrap();
    assert_eq!(fs::read_dir(retired).unwrap().count(), 0);
}
