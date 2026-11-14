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

/// A box whose task list on #6 has issue 7 (the one [`Box::record`] keeps
/// a job's record of) as its current child and #9 after it, with job j1
/// for #7: its one turn awaited `target`, then said it ends with `end`.
/// GitHub has #7 `state`, with `labels`. Its record, and #6.
fn child_job(target: &str, end: &str, state: &str, labels: &[&str]) -> (Box, PathBuf, Value) {
    let mut b = Box::new();
    b.cfg.inbox.as_mut().unwrap().planning = true;
    let mut parent = issue(6, "sam", &["tracker"]);
    parent["state"] = json!("open");
    fs::write(b.path("repos_sam_box_issues_6"), parent.to_string()).unwrap();
    let p = b.pass();
    p.keep_planned("sam/box", "6", &p.issue_text("sam/box", &parent, &[]))
        .unwrap();
    let mut list = List {
        source: None,
        revision: 0,
        retained: vec![],
        tasks: (0..2)
            .map(|i| Task {
                issue: None,
                title: format!("task {i}"),
                body: "one PR".into(),
                check: "regression".into(),
            })
            .collect(),
        children: [7, 9]
            .map(|n| Child {
                number: Some(n),
                offered: true,
                ..Child::default()
            })
            .into(),
        comment: Some(100),
        rendered: String::new(),
    };
    list.rendered = list.checklist("sam/box", "6");
    assert!(p.save_list("sam/box", "6", &list));
    b.job_ending("j1", "done", Some(end), "Landed.");
    fs::write(
        b.cfg.jobs.join("j1/1.github-awaits"),
        format!("{target}\t1000\t{{}}\n"),
    )
    .unwrap();
    let d = b.record("j1");
    let mut now = issue(7, "sam", labels);
    now["state"] = json!(state);
    fs::write(b.path("repos_sam_box_issues_7"), now.to_string()).unwrap();
    (b, d, parent)
}

/// The jobs pass, with #7 open on the list it reads when `open`.
fn jobs(b: &Box, open: bool) {
    let listed: Vec<Value> = if open {
        vec![serde_json::from_slice(&fs::read(b.path("repos_sam_box_issues_7")).unwrap()).unwrap()]
    } else {
        vec![]
    };
    b.pass().jobs("sam/box", "box", &listed).unwrap();
}

fn moved_on(b: &Box) -> bool {
    b.pass().child_admissible("sam/box", "9")
}

/// FieldmouseWorks/Conary#304 and #310: the job awaited its PR, which said
/// `Refs`, in the child's repo or another, and said `proj done` once it
/// merged. No closing reference: the pass accepts the completion, records
/// the PR as the child's landed PR, closes it, and the list moves on.
#[test]
fn a_refs_pr_the_childs_job_awaited_lands_it_in_any_repo() {
    for repo in ["sam/box", "sam/machine"] {
        let (b, d, parent) = child_job(&format!("{repo}#10"), "done", "open", &["claude"]);
        let url = pr(&b, repo, 10, true);
        // Asked alone, nothing is recorded: only an accepted completion is.
        assert_eq!(
            b.pass()
                .child_can_close("sam/box", "7", Some(&crate::job::Job::new(&b.cfg, "j1"))),
            Closing::Record(url.clone())
        );
        assert!(b.pass().recorded_landing("sam/box", 7).is_none(), "{repo}");
        jobs(&b, true);
        assert!(
            b.log()
                .contains("-X PATCH repos/sam/box/issues/7 -f state=closed"),
            "{repo}: {}",
            b.log()
        );
        assert!(!d.exists(), "{repo}");
        assert_eq!(
            b.pass().recorded_landing("sam/box", 7).as_deref(),
            Some(url.as_str())
        );
        let mut closed = issue(7, "sam", &["claude"]);
        closed["state"] = json!("closed");
        fs::write(b.path("repos_sam_box_issues_7"), closed.to_string()).unwrap();
        b.pass().sequence("sam/box", std::slice::from_ref(&parent));
        assert!(moved_on(&b), "{repo}");
    }
}

/// Edited while the job ran: the job is handed the issue again, and its
/// awaited PR isn't the child's landed PR.
#[test]
fn an_edit_while_the_job_ran_records_nothing() {
    let (mut b, d, _) = child_job("sam/box#10", "done", "open", &["claude"]);
    b.cfg.tools.systemd_run = "true".into();
    b.cfg.tools.systemctl = "false".into();
    fs::write(b.cfg.jobs.join("j1/session"), "s1\n").unwrap();
    pr(&b, "sam/box", 10, true);
    let mut now = issue(7, "sam", &["claude"]);
    now["state"] = json!("open");
    now["body"] = json!("It's broken, and the docs need it too.");
    fs::write(b.path("repos_sam_box_issues_7"), now.to_string()).unwrap();
    jobs(&b, true);
    assert!(!b.log().contains("state=closed"), "{}", b.log());
    assert!(d.exists());
    assert!(b.pass().recorded_landing("sam/box", 7).is_none());
}

/// Closed by hand before the pass saw the job finish: the awaited PR is
/// kept before the record goes. Not for a job that hadn't finished, nor
/// one whose issue was edited after it was handed it.
#[test]
fn a_hand_close_after_the_jobs_done_keeps_its_landing() {
    for (end, edited, kept) in [
        ("done", false, true),
        ("await", false, false),
        ("done", true, false),
    ] {
        let (b, d, parent) = child_job("sam/machine#1218", end, "closed", &["claude"]);
        if edited {
            let mut now = issue(7, "sam", &["claude"]);
            now["state"] = json!("closed");
            now["body"] = json!("It's broken, and the docs need it too.");
            fs::write(b.path("repos_sam_box_issues_7"), now.to_string()).unwrap();
        }
        let url = pr(&b, "sam/machine", 1218, true);
        jobs(&b, false);
        assert!(!d.exists(), "{end}");
        assert_eq!(
            b.pass().recorded_landing("sam/box", 7),
            kept.then_some(url),
            "{end} {edited}: {}",
            b.log()
        );
        b.pass().sequence("sam/box", std::slice::from_ref(&parent));
        assert_eq!(moved_on(&b), kept, "{end} {edited}");
    }
}

/// A child that inherited the repair lane closes with its landed PR too.
#[test]
fn a_repair_lane_child_closes_with_its_landing() {
    let (b, _, _) = child_job("sam/box#10", "done", "open", &["claude", "stuck"]);
    let url = pr(&b, "sam/box", 10, true);
    jobs(&b, true);
    assert!(
        b.log()
            .contains("-X PATCH repos/sam/box/issues/7 -f state=closed"),
        "{}",
        b.log()
    );
    assert_eq!(b.pass().recorded_landing("sam/box", 7), Some(url));
}

#[test]
fn an_awaited_pr_that_never_merged_lands_nothing() {
    let (b, d, _) = child_job("sam/box#10", "done", "open", &["claude"]);
    pr(&b, "sam/box", 10, false);
    jobs(&b, true);
    assert!(!b.log().contains("state=closed"), "{}", b.log());
    assert!(d.exists());
    assert!(b.pass().recorded_landing("sam/box", 7).is_none());
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

/// A notifier that keeps one notice per key as a file, and says so with
/// `pending`, as burrow-notify does; delivery fails while `ensure-fails` or
/// `close-fails` is there.
fn notifier(b: &mut Box) {
    let notify = b.path("notify");
    let at = b.path("notices.d");
    fs::create_dir_all(&at).unwrap();
    burrow_test::executable(
        &notify,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$1 $2\" >>'{log}'\ncase \"$1\" in\n  ensure) [ -e '{efails}' ] || touch '{at}'/\"$2\" ;;\n  close) [ -e '{fails}' ] || rm -f '{at}'/\"$2\" ;;\n  pending) [ -e '{at}'/\"$2\" ] && exit 0; exit 1 ;;\nesac\n",
            log = b.path("notices").display(),
            at = at.display(),
            fails = b.path("close-fails").display(),
            efails = b.path("ensure-fails").display(),
        ),
    );
    b.cfg.notify = Some(notify);
}

fn notice_open(b: &Box) -> bool {
    b.path("notices.d/proj-plan-sam_box-7").exists()
}

#[test]
fn a_child_closed_by_hand_waits_and_says_so_once() {
    let (mut b, parent, _) = parent_fixture();
    notifier(&mut b);
    child(&b, "closed");
    // The first pass's notice doesn't get through: a later pass's does.
    fs::write(b.path("ensure-fails"), "").unwrap();
    b.pass().sequence("sam/box", std::slice::from_ref(&parent));
    assert!(!notice_open(&b));
    fs::remove_file(b.path("ensure-fails")).unwrap();
    for _ in 0..2 {
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
    // One notice, ensured each pass: asking again never opens another.
    let notices = fs::read_to_string(b.path("notices")).unwrap();
    assert!(
        notices.lines().all(|l| l == "ensure proj-plan-sam_box-7"),
        "{notices}"
    );
    assert!(notice_open(&b));
    // An author records the PR that landed it: the list moves on, and the
    // notice is closed, the close tried again until it's known to be.
    fs::write(b.path("close-fails"), "").unwrap();
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
    assert!(notice_open(&b));
    fs::remove_file(b.path("close-fails")).unwrap();
    b.pass().sequence("sam/box", std::slice::from_ref(&parent));
    assert!(!notice_open(&b));
    // Known closed: not asked again.
    let before = fs::read_to_string(b.path("notices")).unwrap();
    b.pass().sequence("sam/box", std::slice::from_ref(&parent));
    assert_eq!(fs::read_to_string(b.path("notices")).unwrap(), before);
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
        ("sam/else#8", &landed, Some(author)),
    ] {
        assert!(
            b.pass().plan_done(child_ref, url, caller).is_err(),
            "{child_ref} {url} {caller:?}"
        );
        b.pass().sequence("sam/box", std::slice::from_ref(&parent));
        assert!(!advanced(&b), "{child_ref} {url} {caller:?}");
    }
    assert!(b.pass().recorded_landing("sam/box", 8).is_none());
    // Spelled another way, it's still the clone's repo.
    assert_eq!(
        b.pass()
            .plan_done("Sam/Box#8", &landed, Some(author))
            .unwrap(),
        "sam/box"
    );
    assert_eq!(b.pass().recorded_landing("sam/box", 8), Some(landed));
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
