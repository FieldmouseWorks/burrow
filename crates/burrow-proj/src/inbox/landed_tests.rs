//! A child is done when a merged PR is tied to it, whatever the PR said
//! (landed.rs).
use super::super::sorting_tests::{Box, issue};
use super::tests::parent_fixture;
use super::*;
use crate::inbox::record::files;

/// The child, as GitHub has it now.
pub(super) fn child(b: &Box, state: &str) {
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
pub(super) fn advanced(b: &Box) -> bool {
    b.pass().child_admissible("sam/box", "9")
}

pub(super) fn stored(b: &Box) -> List {
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
                carries: None,
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
    if state == "closed" {
        now["closed_at"] = json!("2026-11-13T20:00:55Z");
    }
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

/// What the host kept of what #7's job awaited, one per line.
fn kept(b: &Box) -> Option<String> {
    let kept: Value = serde_json::from_str(&crate::read(
        &b.pass().plan_dir("sam/box", "7").join("awaited"),
    )?)
    .unwrap();
    Some(
        kept["targets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t.as_str().unwrap())
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// GitHub has #7 closed now, at `at`.
fn closed_at(b: &Box, at: &str) {
    let mut now: Value =
        serde_json::from_slice(&fs::read(b.path("repos_sam_box_issues_7")).unwrap()).unwrap();
    now["state"] = json!("closed");
    now["closed_at"] = json!(at);
    fs::write(b.path("repos_sam_box_issues_7"), now.to_string()).unwrap();
}

fn closed(b: &Box) {
    closed_at(b, "2026-11-13T20:00:55Z");
}

/// When the test GitHub says an issue the pass closed closed.
const CLOSED_BY_PASS: &str = "2026-11-13T21:00:00Z";

/// FieldmouseWorks/Conary#304 and #310: the job awaited its PR, which said
/// `Refs`, in the child's repo or another, and said `proj done` once it
/// merged. No closing reference: the pass accepts the completion, closes
/// the child, keeps what the job awaited, and the list moves on.
#[test]
fn a_refs_pr_the_childs_job_awaited_lands_it_in_any_repo() {
    for repo in ["sam/box", "sam/machine"] {
        let (b, d, parent) = child_job(&format!("{repo}#10"), "done", "open", &["claude"]);
        let url = pr(&b, repo, 10, true);
        // Asked alone, nothing is kept: only a close that went through is.
        assert_eq!(
            b.pass()
                .child_can_close("sam/box", "7", Some(&crate::job::Job::new(&b.cfg, "j1"))),
            Closing::Awaited(url.clone())
        );
        assert_eq!(kept(&b), None, "{repo}");
        jobs(&b, true);
        assert!(
            b.log()
                .contains("-X PATCH repos/sam/box/issues/7 -f state=closed"),
            "{repo}: {}",
            b.log()
        );
        // Kept in the same pass, on the closure GitHub answered the close
        // with, and the job let go: the list is sequenced next in that
        // pass, and says nothing of waiting.
        assert!(!d.exists(), "{repo}");
        assert_eq!(kept(&b), Some(format!("{repo}#10")), "{repo}");
        closed_at(&b, CLOSED_BY_PASS);
        b.pass().sequence("sam/box", std::slice::from_ref(&parent));
        assert!(moved_on(&b), "{repo}");
        assert!(!b.log().contains("the list waits"), "{repo}");
        assert_eq!(
            stored_at(&b, "6").children[0].merged_pr.as_deref(),
            Some(url.as_str())
        );
    }
}

pub(super) fn stored_at(b: &Box, parent: &str) -> List {
    serde_json::from_slice(
        &fs::read(b.pass().plan_dir("sam/box", parent).join("tasks.json")).unwrap(),
    )
    .unwrap()
}

/// Edited while the job ran: the job is handed the issue again, and what it
/// awaited isn't kept.
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
    assert_eq!(kept(&b), None);
}

/// A close that didn't go through keeps nothing: the child stays open, its
/// list where it was, and the job's record stays for the next pass.
#[test]
fn a_failed_close_keeps_no_evidence() {
    let (b, d, parent) = child_job("sam/box#10", "done", "open", &["claude"]);
    pr(&b, "sam/box", 10, true);
    fs::write(b.path("fail"), "state=closed\n").unwrap();
    jobs(&b, true);
    assert!(d.exists());
    assert_eq!(kept(&b), None);
    b.pass().sequence("sam/box", std::slice::from_ref(&parent));
    assert!(!moved_on(&b));
}

/// Closed by hand before the pass saw the job finish: what the job awaited
/// is kept before the record goes. Not for a job that hadn't finished, nor
/// one whose issue was edited after it was handed it, nor a one-way
/// question's timeout, whose synthetic `done` leaves the owner's answer owed.
#[test]
fn a_hand_close_after_the_jobs_done_keeps_its_landing() {
    for (case, end, keeps) in [
        ("done", "done", true),
        ("await", "await", false),
        ("edited", "done", false),
        ("timeout", "done", false),
    ] {
        let (b, d, parent) = child_job("sam/machine#1218", end, "closed", &["claude"]);
        match case {
            "edited" => {
                let mut now = issue(7, "sam", &["claude"]);
                now["state"] = json!("closed");
                now["closed_at"] = json!("2026-11-13T20:00:55Z");
                now["body"] = json!("It's broken, and the docs need it too.");
                fs::write(b.path("repos_sam_box_issues_7"), now.to_string()).unwrap();
            }
            "timeout" => {
                let job = crate::job::Job::new(&b.cfg, "j1");
                fs::write(job.turn("1", "timeout"), "one-way\n").unwrap();
                fs::write(
                    crate::ask::host_path(&job, "question-needs-owner"),
                    "keep it",
                )
                .unwrap();
            }
            _ => {}
        }
        pr(&b, "sam/machine", 1218, true);
        jobs(&b, false);
        assert!(!d.exists(), "{case}");
        assert_eq!(kept(&b).is_some(), keeps, "{case}: {}", b.log());
        b.pass().sequence("sam/box", std::slice::from_ref(&parent));
        assert_eq!(moved_on(&b), keeps, "{case}");
    }
}

/// The awaited PR hadn't merged when the child closed: the list waits, and
/// moves on once it does, with no job left to revive. Reopened and closed
/// again, whether or not a pass saw it open, or edited, the closure isn't
/// the one the job's awaits were kept on, and they don't count.
#[test]
fn an_awaited_pr_that_merges_after_the_close_lands_it_then() {
    let (b, _, parent) = child_job("sam/machine#1218", "done", "closed", &["claude"]);
    pr(&b, "sam/machine", 1218, false);
    jobs(&b, false);
    assert_eq!(kept(&b).as_deref(), Some("sam/machine#1218"));
    let mut p = b.pass();
    p.sequence("sam/box", std::slice::from_ref(&parent));
    assert!(!moved_on(&b));
    assert!(p.wake.contains_key("sam/box"), "a later look is booked");
    // Reopened and closed again by hand, unseen; it merges meanwhile.
    closed_at(&b, "2026-11-14T09:00:00Z");
    pr(&b, "sam/machine", 1218, true);
    b.pass().sequence("sam/box", std::slice::from_ref(&parent));
    assert!(!moved_on(&b));
    // Edited, on the same closure, likewise.
    closed(&b);
    let mut now: Value =
        serde_json::from_slice(&fs::read(b.path("repos_sam_box_issues_7")).unwrap()).unwrap();
    now["title"] = json!("Fix it, and the docs");
    fs::write(b.path("repos_sam_box_issues_7"), now.to_string()).unwrap();
    b.pass().sequence("sam/box", std::slice::from_ref(&parent));
    assert!(!moved_on(&b));
    // Kept again, and merged since.
    let (b, _, parent) = child_job("sam/machine#1218", "done", "closed", &["claude"]);
    pr(&b, "sam/machine", 1218, false);
    jobs(&b, false);
    b.pass().sequence("sam/box", std::slice::from_ref(&parent));
    assert!(!moved_on(&b));
    let url = pr(&b, "sam/machine", 1218, true);
    b.pass().sequence("sam/box", std::slice::from_ref(&parent));
    assert!(moved_on(&b));
    assert_eq!(
        stored_at(&b, "6").children[0].merged_pr.as_deref(),
        Some(url.as_str())
    );
}

/// A child that inherited the repair lane closes with its landed PR too.
#[test]
fn a_repair_lane_child_closes_with_its_landing() {
    let (b, _, _) = child_job("sam/box#10", "done", "open", &["claude", "stuck"]);
    pr(&b, "sam/box", 10, true);
    jobs(&b, true);
    assert!(
        b.log()
            .contains("-X PATCH repos/sam/box/issues/7 -f state=closed"),
        "{}",
        b.log()
    );
    assert_eq!(kept(&b).as_deref(), Some("sam/box#10"));
}

/// The closed child's REST text isn't the text the completion was checked
/// against (GraphQL's): nothing is kept, and the record stays to be read
/// again.
#[test]
fn a_closure_whose_text_wasnt_checked_keeps_nothing() {
    let (b, d, _) = child_job("sam/box#10", "done", "closed", &["claude"]);
    pr(&b, "sam/box", 10, true);
    let mut rest: Value =
        serde_json::from_slice(&fs::read(b.path("repos_sam_box_issues_7")).unwrap()).unwrap();
    rest["body"] = json!("It's broken, and the docs need it too.");
    // Its answer posted, as the jobs pass does first.
    fs::write(d.join(files::File::Answered), "1\n").unwrap();
    let job = crate::job::Job::new(&b.cfg, "j1");
    assert!(!b.pass().keep_landing("sam/box", "7", &job, &d, &rest));
    assert_eq!(kept(&b), None);
    let rest: Value =
        serde_json::from_slice(&fs::read(b.path("repos_sam_box_issues_7")).unwrap()).unwrap();
    assert!(b.pass().keep_landing("sam/box", "7", &job, &d, &rest));
    assert_eq!(kept(&b).as_deref(), Some("sam/box#10"));
}

/// An open child whose closing references can't be read closes all the
/// same by its job's merged awaited PR; with neither proving it, it's a
/// miss.
#[test]
fn a_failed_closing_reference_lookup_never_vetoes_a_live_await() {
    for merged in [true, false] {
        let (b, _, _) = child_job("sam/box#10", "done", "open", &["claude"]);
        let url = pr(&b, "sam/box", 10, merged);
        fs::write(b.path("fail"), "merged-query\n").unwrap();
        let job = crate::job::Job::new(&b.cfg, "j1");
        let mut p = b.pass();
        let closing = p.child_can_close("sam/box", "7", Some(&job));
        if merged {
            assert_eq!(closing, Closing::Awaited(url));
        } else {
            assert_eq!(closing, Closing::No);
            assert!(p.unreached.contains(&"sam/box".to_owned()));
        }
    }
}

/// One awaited target GitHub won't answer for vetoes nothing another
/// source proves: an older target that merged, or a closing reference.
/// With no proof at all, it's a miss, never a merge.
#[test]
fn an_unreadable_await_never_vetoes_other_proof() {
    for proof in ["await", "closing", "none"] {
        let (b, _, parent) = child_job("sam/gone#1", "done", "closed", &["claude"]);
        let job = crate::job::Job::new(&b.cfg, "j1");
        let mut awaits = "sam/gone#1\t1000\t{}\n".to_owned();
        if proof == "await" {
            // Awaited first, so the unreadable one is newer.
            awaits = format!("sam/box#10\t900\t{{}}\n{awaits}");
            pr(&b, "sam/box", 10, true);
        }
        fs::write(job.turn("1", "github-awaits"), awaits).unwrap();
        if proof == "closing" {
            fs::write(
                b.path("merged-7"),
                json!({"data":{"repository":{"issue":{"closedByPullRequestsReferences":{"nodes":[
                    {"merged":true,"url":"https://github.com/sam/box/pull/12"}]}}}}})
                .to_string(),
            )
            .unwrap();
        }
        fs::write(b.path("fail"), "api repos/sam/gone/issues/1\n").unwrap();
        jobs(&b, false);
        assert!(kept(&b).is_some(), "{proof}");
        let mut p = b.pass();
        p.sequence("sam/box", std::slice::from_ref(&parent));
        assert_eq!(moved_on(&b), proof != "none", "{proof}");
        if proof == "none" {
            assert!(p.unreached.contains(&"sam/box".to_owned()), "{proof}");
        }
    }
}

#[test]
fn an_awaited_pr_that_never_merged_lands_nothing() {
    let (b, d, _) = child_job("sam/box#10", "done", "open", &["claude"]);
    pr(&b, "sam/box", 10, false);
    jobs(&b, true);
    assert!(!b.log().contains("state=closed"), "{}", b.log());
    assert!(d.exists());
    assert_eq!(kept(&b), None);
}

/// #6's list says its child #7 was split to carry `target`.
fn carries(b: &Box, target: &str) {
    let mut list = stored_at(b, "6");
    list.tasks[0].carries = Some(target.into());
    assert!(b.pass().save_list("sam/box", "6", &list));
}

/// #7 as GitHub has it now.
fn now(b: &Box) -> Value {
    serde_json::from_slice(&fs::read(b.path("repos_sam_box_issues_7")).unwrap()).unwrap()
}

/// FieldmouseWorks/Conary#450: a child split to carry a PR that said
/// `Refs` merged before its job looked, so the job awaited nothing, and it
/// was closed by hand with no job record left. The PR it carries lands it,
/// with no host step.
#[test]
fn a_child_carrying_a_merged_pr_lands_with_it_closed_by_hand() {
    for repo in ["sam/box", "sam/machine"] {
        let (b, d, parent) = child_job("", "done", "closed", &[]);
        fs::remove_dir_all(&d).unwrap();
        carries(&b, &format!("{repo}#449"));
        let url = pr(&b, repo, 449, true);
        assert_eq!(
            b.pass().landed_child("sam/box", 7, Some(&now(&b))),
            Some(Some(url.clone())),
            "{repo}"
        );
        b.pass().sequence("sam/box", std::slice::from_ref(&parent));
        assert!(moved_on(&b), "{repo}");
        assert_eq!(
            stored_at(&b, "6").children[0].merged_pr.as_deref(),
            Some(url.as_str())
        );
    }
}

/// The carried PR hadn't merged when the child closed: the list waits,
/// looks again later, and moves on once it does. One that never merges
/// lands nothing.
#[test]
fn a_carried_pr_that_merges_after_the_close_lands_it_then() {
    let (b, d, parent) = child_job("", "done", "closed", &[]);
    fs::remove_dir_all(&d).unwrap();
    carries(&b, "sam/machine#449");
    pr(&b, "sam/machine", 449, false);
    let mut p = b.pass();
    p.sequence("sam/box", std::slice::from_ref(&parent));
    assert!(!moved_on(&b));
    assert!(p.wake.contains_key("sam/box"), "a later look is booked");
    let url = pr(&b, "sam/machine", 449, true);
    b.pass().sequence("sam/box", std::slice::from_ref(&parent));
    assert!(moved_on(&b));
    assert_eq!(
        stored_at(&b, "6").children[0].merged_pr.as_deref(),
        Some(url.as_str())
    );
}

/// A carried PR GitHub says isn't there, a mistyped one say, hasn't
/// merged: the list waits on the child and says so, never a silent miss.
#[test]
fn a_carried_pr_that_isnt_there_leaves_the_list_waiting_and_saying_so() {
    let (b, d, parent) = child_job("", "done", "closed", &[]);
    fs::remove_dir_all(&d).unwrap();
    carries(&b, "sam/machine#4490");
    fs::write(b.path("gone"), "api repos/sam/machine/issues/4490\n").unwrap();
    assert_eq!(
        b.pass().landed_child("sam/box", 7, Some(&now(&b))),
        Some(None)
    );
    let mut p = b.pass();
    p.sequence("sam/box", std::slice::from_ref(&parent));
    assert!(!moved_on(&b));
    assert!(!p.unreached.contains(&"sam/box".to_owned()), "{}", b.log());
    assert!(stored_at(&b, "6").children[0].waits);
}

/// The child's job found its carried PR already merged and said `proj
/// done` with nothing awaited: the child closes with its job.
#[test]
fn a_job_that_awaited_nothing_closes_a_child_whose_carried_pr_merged() {
    for merged in [true, false] {
        let (b, d, _) = child_job("", "done", "open", &["claude"]);
        carries(&b, "sam/machine#449");
        pr(&b, "sam/machine", 449, merged);
        let job = crate::job::Job::new(&b.cfg, "j1");
        let closing = b.pass().child_can_close("sam/box", "7", Some(&job));
        assert_eq!(closing.closes(), merged, "{closing:?}");
        jobs(&b, true);
        assert_eq!(
            b.log()
                .contains("-X PATCH repos/sam/box/issues/7 -f state=closed"),
            merged,
            "{}",
            b.log()
        );
        assert_eq!(d.exists(), !merged);
    }
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

/// A notifier as burrow-notify is: a stamp per key, written before the
/// issue is made and dropped once it's closed, which `pending` reads; the
/// issue itself (`notices.d/KEY`) made unless `ensure-fails` is there,
/// closed unless `close-fails` is. Either failing still exits 0.
pub(super) fn notifier(b: &mut Box) {
    let notify = b.path("notify");
    let at = b.path("notices.d");
    fs::create_dir_all(&at).unwrap();
    burrow_test::executable(
        &notify,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$1 $2\" >>'{log}'\ncase \"$1\" in\n  ensure) printf '%s\\n%s\\n' \"$3\" \"$4\" >'{at}'/\"$2\".text; touch '{at}'/\"$2\".stamp; [ -e '{efails}' ] || touch '{at}'/\"$2\" ;;\n  close) [ -e '{fails}' ] || rm -f '{at}'/\"$2\" '{at}'/\"$2\".stamp ;;\n  pending) [ -e '{at}'/\"$2\".stamp ] && exit 0; exit 1 ;;\nesac\n",
            log = b.path("notices").display(),
            at = at.display(),
            fails = b.path("close-fails").display(),
            efails = b.path("ensure-fails").display(),
        ),
    );
    b.cfg.notify = Some(notify);
}

pub(super) fn notice_open(b: &Box) -> bool {
    b.path("notices.d/proj-plan-sam_box-7").exists()
}

/// The title and body the notice was last ensured with.
fn notice_text(b: &Box) -> String {
    fs::read_to_string(b.path("notices.d/proj-plan-sam_box-7.text")).unwrap_or_default()
}

#[test]
fn a_child_closed_by_hand_waits_and_says_so_once() {
    let (mut b, parent, _) = parent_fixture();
    notifier(&mut b);
    child(&b, "closed");
    // The first pass's notice is stamped but never made: a later look is
    // booked all the same, and its ensure makes it.
    fs::write(b.path("ensure-fails"), "").unwrap();
    let mut p = b.pass();
    p.sequence("sam/box", std::slice::from_ref(&parent));
    assert!(!notice_open(&b));
    assert!(p.wake.contains_key("sam/box"), "ensured again later");
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
        notices
            .lines()
            .all(|l| ["ensure proj-plan-sam_box-7", "pending proj-plan-sam_box-7"].contains(&l)),
        "{notices}"
    );
    assert!(notice_open(&b));
    // A scoped pass about another issue says nothing of this parent: its
    // notice stays.
    let mut p = b.pass();
    p.scope = Some(BTreeSet::from([42]));
    p.sequence("sam/box", &[]);
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
    let mut p = b.pass();
    p.sequence("sam/box", std::slice::from_ref(&parent));
    assert!(notice_open(&b));
    assert!(p.wake.contains_key("sam/box"), "the close is tried again");
    // The parent leaves the open list meanwhile: the close still goes on.
    fs::remove_file(b.path("close-fails")).unwrap();
    b.pass().sequence("sam/box", &[]);
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

/// Comments on #8, as GitHub gives them: `body` by `login`, last edited by
/// `editor` when there is one.
fn said_on_child(b: &Box, login: &str, body: &str, editor: Option<&str>) {
    let mut c = json!({"id": 5, "user": burrow_github::fake::actor(login), "body": body,
        "created_at": "2026-11-13T17:22:33Z"});
    if let Some(editor) = editor {
        let by = burrow_github::fake::actor(editor);
        c["userContentEdits"] =
            json!({"totalCount": 1, "nodes": [{"editor": by, "deletedBy": null}]});
    }
    fs::write(
        b.path("repos_sam_box_issues_8_comments"),
        json!([[c]]).to_string(),
    )
    .unwrap();
}

/// The successor, sam/burrow#948, as GitHub has it now.
fn successor(b: &Box, state: &str) {
    let mut it = issue(948, "sam", &[]);
    it["state"] = json!(state);
    fs::write(b.path("repos_sam_burrow_issues_948"), it.to_string()).unwrap();
}

/// A merged PR in sam/burrow closes sam/burrow#948, and only that issue.
fn successor_landed(b: &Box) {
    fs::write(
        b.path("merged-sam_burrow-948"),
        json!({"data":{"repository":{"issue":{"closedByPullRequestsReferences":{"nodes":[
            {"merged":true,"url":"https://github.com/sam/burrow/pull/979"}]}}}}})
        .to_string(),
    )
    .unwrap();
}

pub(super) fn sequence(b: &Box, parent: &Value) {
    b.pass().sequence("sam/box", std::slice::from_ref(parent));
}

#[test]
fn a_child_continued_elsewhere_waits_on_its_successor_and_lands_with_it() {
    let (mut b, parent, _) = parent_fixture();
    notifier(&mut b);
    child(&b, "closed");
    said_on_child(
        &b,
        "sam",
        "Continued as sam/burrow#948, opened in the repo the work lives in.",
        None,
    );
    successor(&b, "open");
    let mut p = b.pass();
    p.sequence("sam/box", std::slice::from_ref(&parent));
    assert!(!advanced(&b));
    assert!(!notice_open(&b), "a live successor is no stall");
    assert!(
        p.wake.contains_key("sam/box"),
        "a merge elsewhere is looked for"
    );
    let list = stored(&b).checklist("sam/box", "7");
    assert!(
        list.contains("[ ] #8: task 0 — continued as sam/burrow#948"),
        "{list}"
    );
    assert!(!list.contains("the list waits"), "{list}");

    successor(&b, "closed");
    successor_landed(&b);
    sequence(&b, &parent);
    assert!(advanced(&b));
    assert!(
        stored(&b)
            .checklist("sam/box", "7")
            .contains("[x] #8: task 0 — https://github.com/sam/burrow/pull/979")
    );
}

#[test]
fn only_an_authors_unedited_comment_continues_a_child() {
    for (by, editor) in [("mallory", None), ("sam", Some("mallory"))] {
        let (mut b, parent, _) = parent_fixture();
        notifier(&mut b);
        child(&b, "closed");
        said_on_child(&b, by, "Continued as sam/burrow#948.", editor);
        successor(&b, "closed");
        successor_landed(&b);
        sequence(&b, &parent);
        assert!(!advanced(&b), "{by} {editor:?}");
        assert!(
            notice_open(&b),
            "closed by hand with no successor: the owner hears"
        );
    }
}

#[test]
fn a_successor_off_this_machine_or_on_its_own_list_is_none() {
    for named in [
        "Continued as elsewhere/repo#948.",
        "Continued as #9.",
        "Continued as #7.",
    ] {
        let (mut b, parent, _) = parent_fixture();
        notifier(&mut b);
        child(&b, "closed");
        said_on_child(&b, "sam", named, None);
        sequence(&b, &parent);
        assert!(!advanced(&b), "{named}");
        assert!(notice_open(&b), "{named}: the list waits and says so");
    }
}

#[test]
fn a_successor_closed_without_a_landing_leaves_the_child_waiting() {
    let (mut b, parent, _) = parent_fixture();
    notifier(&mut b);
    child(&b, "closed");
    said_on_child(&b, "sam", "Continued as sam/burrow#948.", None);
    successor(&b, "open");
    sequence(&b, &parent);
    assert!(!notice_open(&b));
    // It closes with nothing landed: the child waits as a hand close does.
    successor(&b, "closed");
    sequence(&b, &parent);
    assert!(!advanced(&b));
    assert!(notice_open(&b));
    let list = stored(&b).checklist("sam/box", "7");
    assert!(list.contains("the list waits"), "{list}");
    assert!(!list.contains("continued as"), "{list}");
}

#[test]
fn a_successor_that_is_a_pr_lands_when_it_merges() {
    let (mut b, parent, _) = parent_fixture();
    notifier(&mut b);
    child(&b, "closed");
    said_on_child(&b, "sam", "Continued as #950, the PR that does it.", None);
    let open = pr(&b, "sam/box", 950, false);
    let mut p = b.pass();
    p.sequence("sam/box", std::slice::from_ref(&parent));
    assert!(p.unreached.is_empty(), "a PR is no outage");
    assert!(!advanced(&b));
    assert!(!notice_open(&b));
    assert!(
        stored(&b)
            .checklist("sam/box", "7")
            .contains("continued as sam/box#950")
    );
    pr(&b, "sam/box", 950, true);
    sequence(&b, &parent);
    assert!(advanced(&b));
    assert!(
        stored(&b)
            .checklist("sam/box", "7")
            .contains(&format!("[x] #8: task 0 — {open}"))
    );
}

/// Comments on #8 by sam, oldest first, as GitHub gives them.
fn comments_on_child(b: &Box, bodies: &[&str]) {
    let comments: Vec<Value> = bodies
        .iter()
        .zip(5..)
        .map(|(body, id)| {
            json!({"id": id, "user": burrow_github::fake::actor("sam"), "body": body,
                "created_at": "2026-11-13T17:22:33Z"})
        })
        .collect();
    fs::write(
        b.path("repos_sam_box_issues_8_comments"),
        json!([comments]).to_string(),
    )
    .unwrap();
}

#[test]
fn a_continuation_written_on_a_waiting_child_hands_its_place_on_next_pass() {
    let (mut b, parent, _) = parent_fixture();
    notifier(&mut b);
    child(&b, "closed");
    // Its sibling is named first: that hands nothing on, and the notice
    // says why, as well as what would.
    comments_on_child(&b, &["Continued as sam/box#9, the next task."]);
    sequence(&b, &parent);
    assert!(!advanced(&b));
    assert!(stored(&b).children[0].waits);
    let said = notice_text(&b);
    assert!(
        said.contains("names sam/box#9, which is on this task list itself"),
        "{said}"
    );
    assert!(
        said.contains("neither sam/box#7 nor another child of its list"),
        "{said}"
    );
    // Then a merged PR, on the child already closed and waiting: the next
    // pass hands its place on and the notice goes.
    let url = pr(&b, "sam/box", 950, true);
    comments_on_child(
        &b,
        &[
            "Continued as sam/box#9, the next task.",
            "Continued as sam/box#950, which landed what was left.",
        ],
    );
    sequence(&b, &parent);
    assert!(advanced(&b));
    assert!(
        stored(&b)
            .checklist("sam/box", "7")
            .contains(&format!("[x] #8: task 0 — {url}"))
    );
    assert!(!notice_open(&b));
}

#[test]
fn a_parent_changed_after_its_split_pauses_its_list_and_says_so() {
    let (mut b, parent, _) = parent_fixture();
    notifier(&mut b);
    child(&b, "closed");
    let url = pr(&b, "sam/box", 950, true);
    comments_on_child(&b, &["Continued as #950."]);
    let mut changed = parent.clone();
    changed["body"] = json!("a new scope");
    fs::write(b.path("repos_sam_box_issues_7"), changed.to_string()).unwrap();
    for _ in 0..2 {
        sequence(&b, &changed);
    }
    // Paused: no child is read or handed on, and what holds them is the
    // pause, not the closed child.
    assert!(!advanced(&b));
    assert!(!stored(&b).children[0].waits);
    for n in ["8", "9"] {
        let held = b.pass().place("sam/box", n);
        assert!(
            matches!(&held, Some(Place::Stuck(why)) if why.contains("sam/box#7 changed after its split")),
            "{n}: {held:?}"
        );
    }
    let why = b.pass().look().record("sam/box", "7", Some(&changed));
    assert!(
        matches!(&why.wait, Some(crate::inbox::record::Wait::Stuck(w)) if w.contains("changed after its split")),
        "{why:?}"
    );
    assert!(notice_open(&b));
    assert!(notice_text(&b).contains("sam/box#7's task list is paused"));
    assert_eq!(
        b.posted()
            .matches("The parent task changed after its split")
            .count(),
        1,
        "said once on the parent"
    );
    // Its text goes back: the list goes on, the continuation hands #8's
    // place on, and the notice closes.
    fs::write(b.path("repos_sam_box_issues_7"), parent.to_string()).unwrap();
    sequence(&b, &parent);
    assert!(!b.pass().paused("sam/box", "7"));
    assert!(advanced(&b));
    assert!(
        stored(&b)
            .checklist("sam/box", "7")
            .contains(&format!("[x] #8: task 0 — {url}"))
    );
    assert!(!notice_open(&b));
}
