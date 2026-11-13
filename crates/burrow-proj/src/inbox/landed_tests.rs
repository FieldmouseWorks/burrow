//! A child is done when a merged PR is tied to it, whatever the PR said
//! (landed.rs).
use super::super::sorting_tests::{Box, issue};
use super::tests::parent_fixture;
use super::*;

/// The child, as GitHub has it now.
fn child(b: &Box, state: &str) {
    let mut child = issue(8, "sam", &["later"]);
    child["state"] = json!(state);
    fs::write(b.path("repos_sam_box_issues_8"), child.to_string()).unwrap();
}

/// A PR, as REST's issues endpoint gives it: merged or not.
fn pr(b: &Box, repo: &str, n: u64, merged: bool) -> String {
    let url = format!("https://github.com/{repo}/pull/{n}");
    let issue = json!({"number": n, "html_url": url, "pull_request": {
        "html_url": url, "merged_at": if merged { json!("2026-11-12T20:00:00Z") } else { Value::Null }}});
    fs::write(
        b.path(&format!("repos_{}_issues_{n}", repo.replace('/', "_"))),
        issue.to_string(),
    )
    .unwrap();
    url
}

/// Whether the parent's list has moved on to its second child.
fn advanced(b: &Box) -> bool {
    b.pass().child_admissible("sam/box", "9")
}

fn stored(b: &Box) -> List {
    serde_json::from_slice(&fs::read(b.pass().plan_dir("sam/box", "7").join("tasks.json")).unwrap())
        .unwrap()
}

/// The child's job, whose last turn said `proj done` after awaiting `target`
/// in an earlier one.
fn done_job<'b>(b: &'b Box, target: &str) -> crate::job::Job<'b> {
    b.job("j8", "done", "Landed.");
    let d = b.cfg.jobs.join("j8");
    fs::write(d.join("1.github-awaits"), format!("{target}\t1000\t{{}}\n")).unwrap();
    fs::write(d.join("turns"), "2\n").unwrap();
    fs::write(d.join("2.outcome"), "done\n").unwrap();
    fs::write(d.join("2.end"), "done\n").unwrap();
    crate::job::Job::new(&b.cfg, "j8")
}

/// FieldmouseWorks/Conary#304 and #310: the job awaited its PR, which said
/// `Refs`, in the child's repo or another, and said `proj done` once it
/// merged. No closing reference, yet the list moves on.
#[test]
fn a_refs_pr_the_childs_job_awaited_lands_it_in_any_repo() {
    for repo in ["sam/box", "sam/machine"] {
        let (b, parent, _) = parent_fixture();
        child(&b, "open");
        let url = pr(&b, repo, 10, true);
        let job = done_job(&b, &format!("{repo}#10"));
        assert!(!b.pass().child_can_close("sam/box", "8", None), "{repo}");
        assert!(
            b.pass().child_can_close("sam/box", "8", Some(&job)),
            "{repo}"
        );
        // The inbox closes the issue with the job; the list moves on.
        child(&b, "closed");
        b.pass().sequence("sam/box", std::slice::from_ref(&parent));
        assert!(advanced(&b), "{repo}");
        assert_eq!(
            stored(&b).children[0].merged_pr.as_deref(),
            Some(url.as_str())
        );
        // Once recorded, the job's record isn't needed again.
        assert!(b.pass().child_can_close("sam/box", "8", None), "{repo}");
    }
}

#[test]
fn an_awaited_pr_that_never_merged_lands_nothing() {
    let (b, _parent, _) = parent_fixture();
    child(&b, "open");
    pr(&b, "sam/box", 10, false);
    let job = done_job(&b, "sam/box#10");
    assert!(!b.pass().child_can_close("sam/box", "8", Some(&job)));
    assert!(b.pass().recorded_landing("sam/box", 8).is_none());
}

#[test]
fn a_closing_reference_from_another_repo_advances_the_list() {
    let (b, parent, _) = parent_fixture();
    child(&b, "closed");
    fs::write(
        b.path("merged-8"),
        json!({"data":{"repository":{"issue":{"closedByPullRequestsReferences":{"nodes":[
            {"merged":false,"url":"https://github.com/sam/box/pull/9"},
            {"merged":true,"url":"https://github.com/sam/machine/pull/1218"}]}}}}})
        .to_string(),
    )
    .unwrap();
    b.pass().sequence("sam/box", std::slice::from_ref(&parent));
    assert!(advanced(&b));
    assert!(
        stored(&b)
            .checklist("sam/box", "7")
            .contains("[x] #8: task 0 — https://github.com/sam/machine/pull/1218")
    );
}

#[test]
fn a_child_closed_by_hand_waits_and_says_so_once() {
    let (mut b, parent, _) = parent_fixture();
    let notify = b.path("notify");
    burrow_test::executable(
        &notify,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >>'{}'\n",
            b.path("notices").display()
        ),
    );
    b.cfg.notify = Some(notify);
    child(&b, "closed");
    for _ in 0..3 {
        b.pass().sequence("sam/box", std::slice::from_ref(&parent));
    }
    assert!(!advanced(&b));
    let said = |text: &str| {
        b.log()
            .lines()
            .filter(|l| {
                l.contains("-X PATCH repos/sam/box/issues/comments/100") && l.contains(text)
            })
            .count()
    };
    assert_eq!(
        said("#8 closed without a merged PR; the list waits"),
        1,
        "{}",
        b.log()
    );
    let notices = fs::read_to_string(b.path("notices")).unwrap();
    assert_eq!(notices.lines().count(), 1, "{notices}");
    assert!(
        notices.starts_with("open proj-plan-sam_box-7 "),
        "{notices}"
    );
    // An author records the PR that landed it: the list moves on, and the
    // notice clears.
    let url = pr(&b, "sam/machine", 1218, true);
    let author = burrow_github::fake::author("sam").id;
    b.pass().plan_done("sam/box#8", &url, Some(author)).unwrap();
    b.pass().sequence("sam/box", std::slice::from_ref(&parent));
    assert!(advanced(&b));
    assert!(
        !stored(&b)
            .checklist("sam/box", "7")
            .contains("the list waits")
    );
    let notices = fs::read_to_string(b.path("notices")).unwrap();
    assert!(
        notices
            .lines()
            .nth(1)
            .unwrap()
            .starts_with("close proj-plan-sam_box-7"),
        "{notices}"
    );
}

#[test]
fn plan_done_records_only_an_authors_merged_pr() {
    let (b, parent, _) = parent_fixture();
    child(&b, "open");
    let author = burrow_github::fake::author("sam").id;
    let stranger = burrow_github::fake::author("stranger").id;
    let open = pr(&b, "sam/box", 11, false);
    let landed = pr(&b, "sam/box", 10, true);
    for (child_ref, url, caller) in [
        ("sam/box#8", &landed, Some(stranger)),
        ("sam/box#8", &landed, None),
        ("sam/box#8", &open, Some(author)),
        (
            "sam/box#9",
            &"https://github.com/sam/box/issues/10".to_owned(),
            Some(author),
        ),
        ("sam/box#5", &landed, Some(author)),
    ] {
        assert!(
            b.pass().plan_done(child_ref, url, caller).is_err(),
            "{child_ref} {url} {caller:?}"
        );
        b.pass().sequence("sam/box", std::slice::from_ref(&parent));
        assert!(!advanced(&b), "{child_ref} {url} {caller:?}");
    }
    assert!(b.pass().recorded_landing("sam/box", 8).is_none());
    assert_eq!(
        b.pass()
            .plan_done("sam/box#8", &landed, Some(author))
            .unwrap(),
        "sam/box"
    );
    // The child was still open: it's closed with the list moving on.
    b.pass().sequence("sam/box", std::slice::from_ref(&parent));
    assert!(advanced(&b));
    assert!(
        b.log()
            .contains("-X PATCH repos/sam/box/issues/8 -f state=closed"),
        "{}",
        b.log()
    );
}

#[test]
fn a_list_whose_child_is_still_open_is_unchanged() {
    let (b, parent, before) = parent_fixture();
    child(&b, "open");
    // Even a merged PR that references it (merged to another branch, say)
    // doesn't move it on while it's open with nothing recorded.
    fs::write(
        b.path("merged-8"),
        json!({"data":{"repository":{"issue":{"closedByPullRequestsReferences":{"nodes":[
            {"merged":true,"url":"https://github.com/sam/box/pull/10"}]}}}}})
        .to_string(),
    )
    .unwrap();
    b.pass().sequence("sam/box", std::slice::from_ref(&parent));
    b.pass().sequence("sam/box", std::slice::from_ref(&parent));
    assert!(!advanced(&b));
    let after = stored(&b);
    assert!(
        after
            .children
            .iter()
            .all(|c| c.merged_pr.is_none() && !c.waits)
    );
    assert_eq!(after.rendered, before.rendered);
    assert!(!b.log().contains("issues/8 -f state=closed"));
}
