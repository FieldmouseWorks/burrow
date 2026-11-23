//! An issue whose body a bot maintains (Renovate's Dependency Dashboard) is
//! a tracker, never split or planned, and a list split from one ends
//! (maintained.rs); an author ends any other list with `proj plan end`.
use super::super::sorting_tests::{Box, issue};
use super::landed_tests::{notice_open, notifier, stored};
use super::tests::parent_fixture;
use super::*;
use crate::inbox::record::files;

/// Renovate's dashboard as it writes it, naming its open PR `pr`.
fn dashboard_body(pr: u64) -> String {
    format!(
        "This issue lists Renovate updates and detected dependencies. Read the [Dependency Dashboard](https://docs.renovatebot.com/key-concepts/dashboard/) docs to learn more.\n\n## Open\n\nThe following updates have all been created. To force a retry/rebase of any, click on a checkbox below.\n\n - [ ] <!-- rebase-branch=renovate/burrow -->[deps: update burrow](../pull/{pr})\n"
    )
}

/// Issue 7 as Renovate opens its dashboard through the workers App,
/// labelled `labels`, naming PR `pr`; on GitHub as that.
fn dashboard(b: &mut Box, labels: &[&str], pr: u64) -> Value {
    b.cfg.machinery_account = Some(burrow_github::fake::author("workers[bot]"));
    let mut d = issue(7, "workers[bot]", labels);
    d["title"] = json!("Dependency Dashboard");
    d["body"] = json!(dashboard_body(pr));
    d["state"] = json!("open");
    d["repository_url"] = json!("https://api.github.com/repos/sam/box");
    fs::write(b.path("repos_sam_box_issues_7"), d.to_string()).unwrap();
    d
}

/// [`parent_fixture`]'s list, split from Renovate's dashboard as it read
/// naming PR 227, its first child open: the dashboard as GitHub has it.
fn split_dashboard() -> (Box, Value) {
    let (mut b, _, list) = parent_fixture();
    notifier(&mut b);
    let parent = dashboard(&mut b, &["tracker"], 227);
    let p = b.pass();
    let source = p.issue_text("sam/box", &parent, &[]);
    p.keep_planned("sam/box", "7", &source).unwrap();
    assert!(p.save_list("sam/box", "7", &list));
    let mut child = issue(8, "sam", &["later"]);
    child["state"] = json!("open");
    fs::write(b.path("repos_sam_box_issues_8"), child.to_string()).unwrap();
    (b, parent)
}

/// Renovate rewrites its dashboard: PR 248 in place of 227.
fn renovate_edits(b: &mut Box) -> Value {
    dashboard(b, &["tracker"], 248)
}

fn closed_parent(b: &Box) -> bool {
    b.log()
        .contains("-X PATCH repos/sam/box/issues/7 -f state=closed")
}

// Today's bug: Renovate's edit counts as the author's, so the list pauses
// for good and the owner's notice is filed again every pass. A dashboard's
// list ends instead: nothing paused, said or noticed, the dashboard left
// open and its children no longer started, the old list kept on disk.
#[test]
fn a_split_dashboard_renovate_edits_ends_instead_of_pausing() {
    let (mut b, _) = split_dashboard();
    let edited = renovate_edits(&mut b);
    for _ in 0..2 {
        b.pass().sequence("sam/box", std::slice::from_ref(&edited));
    }
    assert!(
        !b.posted().contains("changed after its split"),
        "{}",
        b.posted()
    );
    assert!(!notice_open(&b), "no notice is filed");
    let notices = fs::read_to_string(b.path("notices")).unwrap_or_default();
    assert!(!notices.contains("ensure"), "{notices}");
    assert!(!b.pass().paused("sam/box", "7"));
    assert!(!closed_parent(&b), "the dashboard stays open: {}", b.log());
    let list = stored(&b);
    assert!(list.tasks.is_empty() && list.children.is_empty());
    assert_eq!(list.retained.len(), 2, "its history stays on disk");
    assert!(!b.pass().child_admissible("sam/box", "8"));
    assert!(matches!(
        b.pass().place("sam/box", "8"),
        Some(Place::Superseded(_))
    ));
}

// A dashboard paused before this fix ends the same way, and the notice the
// pause filed is closed.
#[test]
fn a_paused_dashboards_notice_closes_once_its_list_ends() {
    let (mut b, _) = split_dashboard();
    // As a pause left it: said on the parent, the owner's notice open.
    let d = b.pass().plan_dir("sam/box", "7");
    for f in ["paused", "stale-notice", "notice"] {
        fs::write(d.join(f), "paused\n").unwrap();
    }
    for f in ["proj-plan-sam_box-7", "proj-plan-sam_box-7.stamp"] {
        fs::write(b.path("notices.d").join(f), "").unwrap();
    }
    assert!(b.pass().paused("sam/box", "7") && notice_open(&b));
    let edited = renovate_edits(&mut b);
    b.pass().sequence("sam/box", std::slice::from_ref(&edited));
    assert!(!b.pass().paused("sam/box", "7"));
    assert!(!notice_open(&b), "{}", b.log());
    assert!(stored(&b).tasks.is_empty());
}

// Sorting: a dashboard is a tracker without a planner, and Renovate's edits
// never send it back to one, list or no list.
#[test]
fn a_dashboard_is_a_tracker_without_a_planner() {
    let mut b = Box::new();
    b.cfg.inbox.as_mut().unwrap().planning = true;
    let unsorted = dashboard(&mut b, &[], 227);
    b.pass()
        .triage("sam/box", std::slice::from_ref(&unsorted))
        .unwrap();
    assert!(
        b.log().contains("issues/7/labels -f labels[]=tracker"),
        "{}",
        b.log()
    );
    let started = fs::read_dir(&b.cfg.jobs).unwrap().count();
    assert_eq!(started, 0, "no planner job");
    let t = b.cfg.inbox.as_ref().unwrap().triage.clone().unwrap();
    let edited = dashboard(&mut b, &["tracker"], 248);
    assert!(b.pass().sorted("sam/box", &t, &edited, &Default::default()));
    // With the split list it was given before this fix, too.
    let (mut b, _) = split_dashboard();
    let edited = renovate_edits(&mut b);
    assert!(b.pass().sorted("sam/box", &t, &edited, &Default::default()));
}

// A planner that answered `split` on a dashboard (started before this fix)
// creates nothing: the dashboard is sorted a tracker instead.
#[test]
fn a_split_verdict_on_a_dashboard_creates_no_children() {
    let mut b = Box::new();
    b.cfg.inbox.as_mut().unwrap().planning = true;
    let d = dashboard(&mut b, &[], 227);
    let tasks: Vec<Task> = (0..2)
        .map(|i| Task {
            issue: None,
            carries: None,
            title: format!("task {i}"),
            body: "one PR".into(),
            check: "regression".into(),
        })
        .collect();
    b.job(
        "s1",
        "done",
        &format!("split\n{}", serde_json::to_string(&tasks).unwrap()),
    );
    let record = b
        .cfg
        .inbox
        .as_ref()
        .unwrap()
        .state
        .join(".triage/sam_box-7");
    fs::create_dir_all(&record).unwrap();
    fs::write(record.join(files::File::Job), "s1\n").unwrap();
    Pass::hand(&record, &b.pass().issue_text("sam/box", &d, &[])).unwrap();
    b.pass()
        .triage("sam/box", std::slice::from_ref(&d))
        .unwrap();
    assert!(
        !b.log().contains("-X POST repos/sam/box/issues "),
        "{}",
        b.log()
    );
    assert!(!b.pass().has_task_list(Some("sam/box"), "7"));
    assert!(b.log().contains("issues/7/labels -f labels[]=tracker"));
}

// The door: an author ends any list without closing its parent. Its pause
// and notice go, its history stays on disk, its children no longer start,
// and the parent, a tracker, isn't sent back to a planner as it stands.
#[test]
fn an_author_ends_a_paused_list_without_closing_its_parent() {
    let (mut b, mut parent, _) = parent_fixture();
    notifier(&mut b);
    parent["body"] = json!("Changed scope");
    parent["repository_url"] = json!("https://api.github.com/repos/sam/box");
    fs::write(b.path("repos_sam_box_issues_7"), parent.to_string()).unwrap();
    b.pass().sequence("sam/box", std::slice::from_ref(&parent));
    assert!(b.pass().paused("sam/box", "7") && notice_open(&b));
    let author = burrow_github::fake::author("sam").id;
    let stranger = burrow_github::fake::author("stranger").id;
    for (asked, caller) in [
        ("sam/box#7", Some(stranger)),
        ("sam/box#7", None),
        ("else/box#7", Some(author)),
        ("sam/box#8", Some(author)),
        ("sam/box", Some(author)),
    ] {
        assert!(
            b.pass().plan_end(asked, caller).is_err(),
            "{asked} {caller:?}"
        );
    }
    assert_eq!(stored(&b).tasks.len(), 2, "refused, nothing changed");
    assert_eq!(
        b.pass().plan_end("Sam/Box#7", Some(author)).unwrap(),
        "sam/box"
    );
    assert!(!b.pass().paused("sam/box", "7"));
    assert!(!notice_open(&b));
    let list = stored(&b);
    assert!(list.tasks.is_empty() && list.children.is_empty());
    assert_eq!(list.retained.len(), 2);
    assert!(
        b.log().contains("No task is left on it"),
        "its checklist says so: {}",
        b.log()
    );
    for _ in 0..2 {
        b.pass().sequence("sam/box", std::slice::from_ref(&parent));
    }
    assert!(!closed_parent(&b), "{}", b.log());
    assert_eq!(b.posted().matches("changed after its split").count(), 1);
    assert!(!b.pass().child_admissible("sam/box", "8"));
    let t = b.cfg.inbox.as_ref().unwrap().triage.clone().unwrap();
    assert!(b.pass().sorted("sam/box", &t, &parent, &Default::default()));
    assert!(
        b.pass().plan_end("sam/box#7", Some(author)).is_err(),
        "nothing left to end"
    );
}

/// Parent 7's list, as [`parent_fixture`] has it, paused by an edit, its
/// notice open: the parent as GitHub has it.
fn paused_list() -> (Box, Value) {
    let (mut b, mut parent, _) = parent_fixture();
    notifier(&mut b);
    parent["body"] = json!("Changed scope");
    parent["repository_url"] = json!("https://api.github.com/repos/sam/box");
    fs::write(b.path("repos_sam_box_issues_7"), parent.to_string()).unwrap();
    b.pass().sequence("sam/box", std::slice::from_ref(&parent));
    assert!(b.pass().paused("sam/box", "7") && notice_open(&b));
    (b, parent)
}

// An end GitHub cuts short (its checklist, here) stays begun: each pass,
// or asking again, carries it through, rather than leave a list with no
// task, still paused, its notice open and nothing to retry it.
#[test]
fn an_end_github_cuts_short_is_finished_later() {
    let (b, parent) = paused_list();
    let author = burrow_github::fake::author("sam").id;
    fs::write(b.path("fail"), "issues/comments/100\n").unwrap();
    assert!(b.pass().plan_end("sam/box#7", Some(author)).is_err());
    assert!(stored(&b).tasks.is_empty(), "part of it held");
    assert!(b.pass().ending("sam/box", "7"));
    assert!(b.pass().paused("sam/box", "7") && notice_open(&b));
    b.pass().sequence("sam/box", std::slice::from_ref(&parent));
    assert!(b.pass().ending("sam/box", "7"), "GitHub still refuses");
    let again = b.pass().plan_end("sam/box#7", Some(author)).unwrap_err();
    assert!(
        !again.say.unwrap_or_default().contains("no task list"),
        "asking again isn't refused"
    );
    fs::remove_file(b.path("fail")).unwrap();
    b.pass().sequence("sam/box", std::slice::from_ref(&parent));
    assert!(!b.pass().ending("sam/box", "7"));
    assert!(!b.pass().paused("sam/box", "7"));
    assert!(!notice_open(&b));
    assert!(b.log().contains("No task is left on it"));
    let t = b.cfg.inbox.as_ref().unwrap().triage.clone().unwrap();
    assert!(
        b.pass().sorted("sam/box", &t, &parent, &Default::default()),
        "the text it ended on is kept"
    );
}

// Nothing ends a list something could still write back: the parent's own
// job with a split pending, or a sorting verdict being carried out. The
// door refuses, and a dashboard's list waits for it.
#[test]
fn a_list_something_could_still_write_isnt_ended() {
    let author = burrow_github::fake::author("sam").id;
    for writer in ["worker", "sorting"] {
        for dash in [false, true] {
            let (b, parent) = if dash {
                let (mut b, _) = split_dashboard();
                let edited = renovate_edits(&mut b);
                (b, edited)
            } else {
                paused_list()
            };
            let state = b.cfg.inbox.as_ref().unwrap().state.clone();
            let d = match writer {
                "worker" => files::worker(&state, "sam_box", "7"),
                _ => files::sorting(&state, "sam_box", "7"),
            };
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join(files::File::Job), "w1\n").unwrap();
            if writer == "worker" {
                let at = b.pass().plan_dir("sam/box", "7").join("worker-split");
                fs::write(at, "pending\n").unwrap();
            }
            if dash {
                b.pass().sequence("sam/box", std::slice::from_ref(&parent));
            } else {
                let refused = b.pass().plan_end("sam/box#7", Some(author)).unwrap_err();
                assert!(
                    refused.say.unwrap_or_default().contains("nothing changed"),
                    "{writer}"
                );
            }
            assert_eq!(stored(&b).tasks.len(), 2, "{writer} {dash}");
            assert!(!b.pass().ending("sam/box", "7"), "{writer} {dash}");
        }
    }
}
