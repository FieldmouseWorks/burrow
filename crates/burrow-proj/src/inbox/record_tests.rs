//! An issue's record ([`super::record`]): one reading of what the inbox keeps
//! of an issue, which every reader asks and asking never changes, and
//! `proj why OWNER/REPO#N`.

use crate::inbox::record::files;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::json;

use super::record::{Stage, Unseen, Wait};
use super::sorting_tests::{Box, issue};
use crate::job::Job;
use crate::snapshot::SlotKind;

fn state(b: &Box) -> PathBuf {
    b.cfg.inbox.as_ref().unwrap().state.clone()
}

/// A sorting record for sam/box#N whose job is `id`.
fn sorting(b: &Box, n: u64, id: &str) -> PathBuf {
    let d = state(b).join(".triage").join(format!("sam_box-{n}"));
    fs::create_dir_all(&d).unwrap();
    fs::write(d.join(files::File::Job), format!("{id}\n")).unwrap();
    d
}

/// Every file under `dir`, and what it holds.
fn files(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut all = BTreeMap::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        for e in fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                todo.push(p);
            } else if p.file_name().is_some_and(|f| f != "lock") {
                all.insert(p.clone(), fs::read(&p).unwrap());
            }
        }
    }
    all
}

fn sorting_held(b: &Box) -> bool {
    super::view::view(&b.cfg, 15)
        .slots
        .iter()
        .any(|s| s.kind == SlotKind::Sorting && s.holder.is_some())
}

// The 2026-11-13 case: a planner has finished, and an answer to its
// question was accepted, but the cleanup that retires the question hasn't
// run. The pass used to recover the question as it counted, and freed the
// slot; the view only read, and held it. Now both read one record, which
// takes the accepted answer as an answer, and asking changes nothing:
// recovering is the pass's settling step.
#[test]
fn the_view_and_the_pass_agree_on_a_finished_planners_slot_and_asking_changes_nothing() {
    let b = Box::new();
    b.job("s1", "done", "ready high\nClear.");
    let job = Job::new(&b.cfg, "s1");
    let question = crate::ask::host_path(&job, "question");
    fs::write(&question, "{}").unwrap();
    fs::write(
        crate::ask::host_path(&job, "resume-2"),
        json!({"through": 1, "answered": true, "clear": true, "accepted": true,
               "applied": false, "question": "initial"})
        .to_string(),
    )
    .unwrap();
    let d = sorting(&b, 3, "s1");
    let before = (files(&b.cfg.jobs), files(&state(&b)));
    for _ in 0..2 {
        assert!(!b.pass().holds_sorting_slot(&d), "the pass");
        assert!(!sorting_held(&b), "the view");
        let p = b.pass();
        let r = p.look().record("sam/box", "3", None);
        assert_eq!(r.stage, Stage::Sorting);
        assert_eq!(r.wait, Some(Wait::Pass), "{r:?}");
        assert_eq!(
            (files(&b.cfg.jobs), files(&state(&b))),
            before,
            "asking wrote"
        );
    }
    // Recovering is a step the pass takes, before it asks GitHub anything.
    b.pass().settle();
    assert!(!question.exists(), "the pass's settling step recovers it");
    assert!(!b.pass().holds_sorting_slot(&d));
}

#[test]
fn each_stage_an_issue_can_be_in_derives_to_its_record() {
    let check =
        |b: &Box, n: u64, given: Option<serde_json::Value>, stage: Stage, wait: Option<Wait>| {
            let p = b.pass();
            let r = p.look().record("sam/box", &n.to_string(), given.as_ref());
            assert_eq!(
                (r.stage.clone(), r.wait.clone()),
                (stage, wait),
                "#{n}: {r:?}"
            );
            assert!(!r.next.is_empty(), "#{n} says what's next");
            r
        };
    let b = Box::new();

    // Unseen, each for its reason.
    check(&b, 7, None, Stage::Unseen(Unseen::Unknown), None);
    check(
        &b,
        7,
        Some(issue(7, "stranger", &["claude"])),
        Stage::Unseen(Unseen::NotAuthor("stranger".into())),
        None,
    );
    check(
        &b,
        7,
        Some(issue(7, "sam", &["claude", "outside text"])),
        Stage::Unseen(Unseen::Outside("outside text".into())),
        None,
    );
    check(
        &b,
        7,
        Some(issue(7, "sam", &[])),
        Stage::Unseen(Unseen::NoLabel),
        None,
    );
    for label in ["later", "tracker", "needs-owner", "probe"] {
        check(
            &b,
            7,
            Some(issue(7, "sam", &[label])),
            Stage::Unseen(Unseen::Label(label.into())),
            None,
        );
    }
    let mut closed = issue(7, "sam", &["claude"]);
    closed["state"] = json!("closed");
    check(&b, 7, Some(closed), Stage::Closed, None);
    let refused = state(&b).join("sam_box-8");
    fs::create_dir_all(&refused).unwrap();
    fs::write(refused.join(files::File::Refused), "").unwrap();
    check(&b, 8, None, Stage::Unseen(Unseen::Refused), None);
    let unreached = state(&b).join(".unreached");
    fs::create_dir_all(&unreached).unwrap();
    fs::write(unreached.join("sam_box"), "1793491200\n").unwrap();
    check(
        &b,
        9,
        None,
        Stage::Unseen(Unseen::Unlisted(1_793_491_200)),
        Some(Wait::Pass),
    );

    // Sorting: waiting for a slot, its job at it, its verdict to carry out.
    let waits = state(&b).join(".triage-waiting");
    fs::create_dir_all(&waits).unwrap();
    fs::write(
        waits.join("sam_box-10"),
        "sam/box#10: waiting for a planner slot",
    )
    .unwrap();
    check(&b, 10, None, Stage::Sorting, Some(Wait::SortingSlot));
    b.job("s11", "done", "ready high");
    fs::remove_file(b.cfg.jobs.join("s11/1.outcome")).unwrap();
    fs::write(b.path("active"), "").unwrap();
    sorting(&b, 11, "s11");
    let r = check(&b, 11, None, Stage::Sorting, Some(Wait::Job("s11".into())));
    assert_eq!(r.job.as_deref(), Some("s11"));
    fs::remove_file(b.path("active")).unwrap();
    fs::write(b.cfg.jobs.join("s11/1.outcome"), "done\n").unwrap();
    fs::write(sorting(&b, 11, "s11").join(files::File::Plan), "[]").unwrap();
    check(&b, 11, None, Stage::Sorting, Some(Wait::Verdict));

    // Ready, from its repo's ready list.
    let mut ready = issue(12, "sam", &["ready"]);
    ready["repository_url"] = json!("https://api.github.com/repos/sam/box");
    ready["created_at"] = json!("2026-11-01T00:00:00Z");
    let lists = state(&b).join(".ready");
    fs::create_dir_all(&lists).unwrap();
    fs::write(lists.join("sam_box"), json!([ready]).to_string()).unwrap();
    check(&b, 12, None, Stage::Ready, None);
    assert_eq!(
        b.pass()
            .next_ready()
            .iter()
            .map(|c| (c.repo.clone(), c.n.clone()))
            .collect::<Vec<_>>(),
        [("sam/box".to_owned(), "12".to_owned())],
        "the drain's order, which the snapshot's queue prints, is the ready records'"
    );

    // Working, its answer to post, answered.
    b.job("j1", "done", "Fixed.");
    fs::remove_file(b.cfg.jobs.join("j1/1.outcome")).unwrap();
    fs::write(b.path("active"), "").unwrap();
    let d = b.record("j1");
    check(&b, 7, None, Stage::Working, Some(Wait::Job("j1".into())));
    fs::remove_file(b.path("active")).unwrap();
    fs::write(b.cfg.jobs.join("j1/1.outcome"), "done\n").unwrap();
    check(&b, 7, None, Stage::Working, Some(Wait::Pass));
    fs::write(d.join(files::File::Answered), "1\n").unwrap();
    check(&b, 7, None, Stage::Answered, None);
    fs::write(b.cfg.jobs.join("j1/1.end"), "ask\n").unwrap();
    check(
        &b,
        7,
        None,
        Stage::Answered,
        Some(Wait::Answer("j1".into())),
    );
}

#[test]
fn a_planned_parent_waits_on_its_next_child_and_a_later_child_on_it() {
    let mut b = Box::new();
    b.cfg.inbox.as_mut().unwrap().planning = true;
    let plans = state(&b).join(".plans").join("sam_box-20");
    fs::create_dir_all(&plans).unwrap();
    let task = json!({"title": "t", "body": "b", "check": "c"});
    let child =
        |n: u64, merged: Option<&str>| json!({"number": n, "attempted": true, "merged_pr": merged});
    fs::write(
        plans.join("tasks.json"),
        json!({"tasks": [task, task, task],
               "children": [child(21, Some("https://github.com/sam/box/pull/1")),
                            child(22, None), child(23, None)],
               "comment": null, "rendered": ""})
        .to_string(),
    )
    .unwrap();
    let p = b.pass();
    let look = p.look();
    let parent = look.record("sam/box", "20", None);
    assert_eq!(parent.stage, Stage::Planned);
    assert_eq!(parent.wait, Some(Wait::Plan("sam/box#22".into())));
    let mut later = issue(23, "sam", &["ready"]);
    later["repository_url"] = json!("https://api.github.com/repos/sam/box");
    let behind = look.record("sam/box", "23", Some(&later));
    assert_eq!(behind.stage, Stage::Ready);
    assert_eq!(behind.wait, Some(Wait::Plan("sam/box#22".into())));
}

// A sorting job ended without an answer, and an author then labelled the
// issue `ready` themselves. The pass never sorts it again, so it keeps the
// failed record; the record gives way to the label, and the drain hands the
// issue on, as its ranking did before there was a record.
#[test]
fn a_ready_label_outranks_a_sorting_that_ended_without_an_answer() {
    let b = Box::new();
    let d = state(&b).join(".triage").join("sam_box-14");
    fs::create_dir_all(&d).unwrap();
    fs::write(d.join(files::File::Failed), "2026-11-01T00:00:00Z").unwrap();
    let p = b.pass();
    let r = p.look().record("sam/box", "14", None);
    assert_eq!(
        (r.stage, r.wait),
        (Stage::Sorting, None),
        "unlabelled, it waits"
    );

    let mut ready = issue(14, "sam", &["ready"]);
    ready["repository_url"] = json!("https://api.github.com/repos/sam/box");
    ready["created_at"] = json!("2026-11-01T00:00:00Z");
    let lists = state(&b).join(".ready");
    fs::create_dir_all(&lists).unwrap();
    fs::write(lists.join("sam_box"), json!([ready]).to_string()).unwrap();
    let p = b.pass();
    let look = p.look();
    for given in [None, Some(&ready)] {
        let r = look.record("sam/box", "14", given);
        assert_eq!(
            (r.stage.clone(), r.wait.clone()),
            (Stage::Ready, None),
            "{r:?}"
        );
    }
    assert!(look.drains("sam/box", &ready));
    assert_eq!(
        b.pass()
            .next_ready()
            .iter()
            .map(|c| (c.repo.clone(), c.n.clone()))
            .collect::<Vec<_>>(),
        [("sam/box".to_owned(), "14".to_owned())],
    );
}

#[test]
fn proj_why_names_the_wait_and_what_ends_it_and_follows_the_issue_into_its_job() {
    let b = Box::new();
    b.job("j1", "done", "Fixed.");
    fs::remove_file(b.cfg.jobs.join("j1/1.outcome")).unwrap();
    fs::write(b.path("active"), "").unwrap();
    b.record("j1");
    let said = b.pass().why("sam/box", "7", false);
    assert!(said.starts_with("sam/box#7 is working, since "), "{said}");
    assert!(
        said.contains("It waits on job j1, which ends with j1's turn ending"),
        "{said}"
    );
    assert!(said.contains("\nIts job, j1:\nj1 is queued"), "{said}");
    let json: serde_json::Value =
        serde_json::from_str(&b.pass().why("sam/box", "7", true)).unwrap();
    assert_eq!(json["stage"], "working");
    assert_eq!(json["job"], "j1");
    assert_eq!(json["record"]["id"], "j1");
}

// The #915 case: a child the workers App opened was refused, and nothing
// said why it never started. Its record says so.
#[test]
fn proj_why_names_why_an_issue_never_enrolled() {
    let b = Box::new();
    fs::write(
        b.path("repos_sam_box_issues_9"),
        json!({"number": 9, "state": "open", "title": "a child", "body": "",
               "user": burrow_github::fake::actor("workers[bot]"),
               "labels": [{"name": "claude"}, {"name": "ready"}]})
        .to_string(),
    )
    .unwrap();
    let said = b.pass().why("sam/box", "9", false);
    assert!(
        said.contains(
            "is unseen: the inbox hasn't taken it up, because it was opened by `workers[bot]`, who isn't one of the inbox's authors"
        ),
        "{said}"
    );
    // GitHub refusing: what's kept here says so, without asking it.
    let unreached = state(&b).join(".unreached");
    fs::create_dir_all(&unreached).unwrap();
    fs::write(unreached.join("sam_box"), "1793491200\n").unwrap();
    let said = b.pass().why("sam/box", "10", false);
    assert!(
        said.contains("because GitHub has refused to list its repo since"),
        "{said}"
    );
    assert!(said.contains("It waits on the inbox's next pass"), "{said}");
}

/// sam/box#P's task list: its children by number (None: not opened yet),
/// those landed, and the children an earlier plan had and this one dropped.
fn plan(b: &Box, p: u64, children: &[(Option<u64>, bool)], dropped: &[u64]) {
    let d = state(b).join(".plans").join(format!("sam_box-{p}"));
    fs::create_dir_all(&d).unwrap();
    let task = json!({"title": "t", "body": "b", "check": "c"});
    let child = |n: Option<u64>, landed: bool| {
        json!({"number": n, "attempted": n.is_some(),
               "merged_pr": landed.then_some("https://github.com/sam/box/pull/1")})
    };
    fs::write(
        d.join("tasks.json"),
        json!({"tasks": children.iter().map(|_| task.clone()).collect::<Vec<_>>(),
               "children": children.iter().map(|(n, l)| child(*n, *l)).collect::<Vec<_>>(),
               "retained": dropped.iter().map(|n| json!({"task": task, "child": child(Some(*n), false)})).collect::<Vec<_>>(),
               "comment": null, "rendered": ""})
        .to_string(),
    )
    .unwrap();
}

/// Issue N as a ready list or GitHub has it, labelled so.
fn listed(n: u64, labels: &[&str]) -> serde_json::Value {
    let mut i = issue(n, "sam", labels);
    i["repository_url"] = json!("https://api.github.com/repos/sam/box");
    i["created_at"] = json!("2026-11-01T00:00:00Z");
    i
}

fn ready_list(b: &Box, issues: &[&serde_json::Value]) {
    let lists = state(b).join(".ready");
    fs::create_dir_all(&lists).unwrap();
    fs::write(lists.join("sam_box"), json!(issues).to_string()).unwrap();
}

fn ranked(b: &Box) -> Vec<String> {
    b.pass().next_ready().into_iter().map(|c| c.n).collect()
}

// A split parent whose replan ended without an answer, labelled `ready`:
// its task list still holds it, so it's planned and the drain passes it
// over, as it did before there was a record. A parent replanned to an
// approved empty list has no plan holding it, and goes on as ready.
#[test]
fn a_planned_parent_stays_planned_when_its_replan_ends_without_an_answer() {
    let mut b = Box::new();
    b.cfg.inbox.as_mut().unwrap().planning = true;
    plan(&b, 20, &[(Some(21), false), (Some(22), false)], &[]);
    let failed = state(&b).join(".triage").join("sam_box-20");
    fs::create_dir_all(&failed).unwrap();
    fs::write(failed.join(files::File::Failed), "2026-11-01T00:00:00Z").unwrap();
    let ready = listed(20, &["ready"]);
    ready_list(&b, &[&ready]);
    let p = b.pass();
    let look = p.look();
    for given in [None, Some(&ready)] {
        let r = look.record("sam/box", "20", given);
        assert_eq!(
            (r.stage.clone(), r.wait.clone()),
            (Stage::Planned, Some(Wait::Plan("sam/box#21".into()))),
            "{r:?}"
        );
    }
    assert!(!look.drains("sam/box", &ready));
    assert!(
        ranked(&b).is_empty(),
        "neither the drain nor the snapshot's queue has it"
    );

    plan(&b, 20, &[], &[]);
    let p = b.pass();
    let r = p.look().record("sam/box", "20", Some(&ready));
    assert_eq!((r.stage, r.wait), (Stage::Ready, None));
    assert_eq!(ranked(&b), ["20"]);
}

// What the inbox keeps of an issue no job holds (a task list its close left
// behind, a refusal, a sorting that ended) gives way to GitHub saying it's
// closed; a reopened issue reads by what's kept again, and a sorting job
// still at it keeps it sorting. `proj why` asks GitHub about any issue no
// job holds.
#[test]
fn a_closed_issue_reads_closed_whatever_inert_records_it_keeps() {
    let mut b = Box::new();
    b.cfg.inbox.as_mut().unwrap().planning = true;
    let closed = |n: u64| {
        let mut i = listed(n, &["claude"]);
        i["state"] = json!("closed");
        i
    };
    let opened = |n: u64| {
        let mut i = listed(n, &["claude"]);
        i["state"] = json!("open");
        i
    };
    // Every child landed: the list closes the parent, and keeps tasks.json.
    plan(&b, 20, &[(Some(21), true)], &[]);
    let refused = state(&b).join("sam_box-8");
    fs::create_dir_all(&refused).unwrap();
    fs::write(refused.join(files::File::Refused), "").unwrap();
    let failed = state(&b).join(".triage").join("sam_box-14");
    fs::create_dir_all(&failed).unwrap();
    fs::write(failed.join(files::File::Failed), "2026-11-01T00:00:00Z").unwrap();
    let p = b.pass();
    let look = p.look();
    for (n, kept) in [
        (20, Stage::Planned),
        (8, Stage::Unseen(Unseen::Refused)),
        (14, Stage::Sorting),
    ] {
        let at = n.to_string();
        assert_eq!(look.record("sam/box", &at, None).stage, kept, "#{n}");
        assert_eq!(
            look.record("sam/box", &at, Some(&closed(n))).stage,
            Stage::Closed,
            "#{n}"
        );
        assert_ne!(
            look.record("sam/box", &at, Some(&opened(n))).stage,
            Stage::Closed,
            "#{n} reopened"
        );
    }
    b.job("s14", "done", "ready high");
    fs::remove_file(b.cfg.jobs.join("s14/1.outcome")).unwrap();
    fs::write(b.path("active"), "").unwrap();
    sorting(&b, 14, "s14");
    let r = b.pass().look().record("sam/box", "14", Some(&closed(14)));
    assert_eq!(r.stage, Stage::Sorting, "its sorting job is still at it");
    fs::remove_file(b.path("active")).unwrap();

    fs::write(b.path("repos_sam_box_issues_20"), closed(20).to_string()).unwrap();
    let said = b.pass().why("sam/box", "20", false);
    assert!(said.starts_with("sam/box#20 is closed"), "{said}");
    fs::write(b.path("repos_sam_box_issues_20"), opened(20).to_string()).unwrap();
    let said = b.pass().why("sam/box", "20", false);
    assert!(said.starts_with("sam/box#20 is planned"), "{said}");
}

// A child labelled with an engine is held by its plan's admission just as
// a `ready` one is: an earlier sibling, its parent's own live job, its list
// having dropped it, and an ancestor's place. What its record says agrees
// with what admission does.
#[test]
fn an_engine_labelled_child_waits_on_what_its_plan_admission_does() {
    let mut b = Box::new();
    b.cfg.inbox.as_mut().unwrap().planning = true;
    plan(&b, 20, &[(Some(21), false), (Some(22), false)], &[25]);
    // #22 is itself split: its child #31 waits as #22 does.
    plan(&b, 22, &[(Some(31), false)], &[]);
    let says = |b: &Box, n: u64| {
        let mut p = b.pass();
        p.observe = true;
        let admitted = p.child_admissible("sam/box", &n.to_string());
        let r = p
            .look()
            .record("sam/box", &n.to_string(), Some(&listed(n, &["claude"])));
        (r.stage, r.wait, admitted)
    };
    assert_eq!(
        says(&b, 21),
        (Stage::Unseen(Unseen::Unknown), Some(Wait::Pass), true),
        "its list's next"
    );
    assert_eq!(
        says(&b, 22),
        (Stage::Planned, Some(Wait::Plan("sam/box#31".into())), false),
        "a parent itself: its list waits on its own child"
    );
    assert_eq!(
        says(&b, 31),
        (Stage::Planned, Some(Wait::Plan("sam/box#21".into())), false),
        "behind its parent's own place"
    );
    assert_eq!(says(&b, 25), (Stage::Planned, None, false), "dropped");
    plan(&b, 40, &[(Some(41), false), (Some(42), false)], &[]);
    assert_eq!(
        says(&b, 42),
        (Stage::Planned, Some(Wait::Plan("sam/box#41".into())), false),
        "behind an earlier sibling"
    );
    let r = b
        .pass()
        .look()
        .record("sam/box", "25", Some(&listed(25, &["claude"])));
    assert!(r.next.contains("replanned without it"), "{}", r.next);

    // Its parent's own job is still at it.
    plan(&b, 7, &[(Some(8), false)], &[]);
    b.job("j1", "done", "Split.");
    fs::remove_file(b.cfg.jobs.join("j1/1.outcome")).unwrap();
    fs::write(b.path("active"), "").unwrap();
    b.record("j1");
    assert_eq!(
        says(&b, 8),
        (Stage::Planned, Some(Wait::Job("j1".into())), false)
    );
    // `ready`-labelled, the same holds.
    let r = b
        .pass()
        .look()
        .record("sam/box", "8", Some(&listed(8, &["ready"])));
    assert_eq!(
        (r.stage, r.wait),
        (Stage::Ready, Some(Wait::Job("j1".into())))
    );
}

// What `proj why` says of a ready issue is what the drain's admission
// does: an enrolment waiting for its first job doesn't wait on itself, a
// full drain holds an ordinary candidate but not a repair, a busy engine
// holds it on that engine, and with the drain off nothing hands it on.
#[test]
fn proj_why_says_the_drains_own_admission() {
    let mut b = Box::new();
    let drained = state(&b).join("sam_box-7");
    fs::create_dir_all(&drained).unwrap();
    fs::write(drained.join(files::File::Drained), "sam/box#7\n").unwrap();
    fs::write(
        b.path("repos_sam_box_issues_7"),
        listed(7, &["claude"]).to_string(),
    )
    .unwrap();
    let said = b.pass().why("sam/box", "7", false);
    assert!(said.starts_with("sam/box#7 is ready"), "{said}");
    assert!(said.contains("It waits on nothing."), "{said}");
    assert!(!said.contains("room among the drain's jobs"), "{said}");

    let issue12 = |labels: &[&str]| {
        fs::write(
            b.path("repos_sam_box_issues_12"),
            listed(12, labels).to_string(),
        )
        .unwrap()
    };
    issue12(&["ready"]);
    let said = b.pass().why("sam/box", "12", false);
    assert!(
        said.contains("It waits on room among the drain's jobs"),
        "#7's enrolment fills drainJobs=1: {said}"
    );
    issue12(&["ready", "stuck"]);
    let said = b.pass().why("sam/box", "12", false);
    assert!(said.contains("It waits on nothing."), "{said}");
    assert!(
        said.contains("a repair goes past the drain's cap"),
        "{said}"
    );

    fs::remove_dir_all(&drained).unwrap();
    issue12(&["ready"]);
    let said = b.pass().why("sam/box", "12", false);
    assert!(
        said.contains("Next: the drain's next pass hands it on to claude"),
        "{said}"
    );
    let slots = b.cfg.jobs.join(".slots");
    fs::create_dir_all(&slots).unwrap();
    let held: Vec<fs::File> = (1..=b.cfg.max_jobs)
        .map(|i| {
            let f = fs::File::create(slots.join(i.to_string())).unwrap();
            f.lock().unwrap();
            f
        })
        .collect();
    let said = b.pass().why("sam/box", "12", false);
    assert!(said.contains("It waits on a free claude slot"), "{said}");
    drop(held);

    b.cfg.inbox.as_mut().unwrap().triage.as_mut().unwrap().drain = None;
    let said = b.pass().why("sam/box", "12", false);
    assert!(said.contains("the drain is off here"), "{said}");
}

// A sorting job's wait comes from its own record, whether or not it holds
// a sorting slot: an expired one-way question waits on the owner alone,
// with no deadline to end it, and a parked job on its park, as the park
// says. An issue's own job reads the same.
#[test]
fn a_jobs_question_and_park_are_what_its_issue_waits_on() {
    let b = Box::new();
    b.job("s3", "done", "ready high");
    fs::write(b.cfg.jobs.join("s3/1.end"), "ask\n").unwrap();
    let job = Job::new(&b.cfg, "s3");
    fs::write(crate::ask::host_path(&job, "question"), "{}").unwrap();
    fs::write(crate::ask::host_path(&job, "question-needs-owner"), "1\n").unwrap();
    let d = sorting(&b, 3, "s3");
    assert!(
        !b.pass().holds_sorting_slot(&d),
        "a question left for the owner frees its slot"
    );
    let r = b.pass().look().record("sam/box", "3", None);
    assert_eq!(
        (r.stage, r.wait.clone()),
        (Stage::Sorting, Some(Wait::Owner("s3".into())))
    );
    let ends = r.wait.unwrap().ends();
    assert!(!ends.contains("or the question's deadline"), "{ends}");

    b.job("j1", "done", "Fixed.");
    fs::write(b.cfg.jobs.join("j1/1.end"), "ask\n").unwrap();
    let w = Job::new(&b.cfg, "j1");
    fs::write(crate::ask::host_path(&w, "question"), "{}").unwrap();
    fs::write(crate::ask::host_path(&w, "question-needs-owner"), "1\n").unwrap();
    let d = b.record("j1");
    fs::write(d.join(files::File::Answered), "1\n").unwrap();
    let r = b.pass().look().record("sam/box", "7", None);
    assert_eq!(
        (r.stage, r.wait),
        (Stage::Answered, Some(Wait::Owner("j1".into())))
    );

    // A turn that runs on keeps an older question open: the issue still
    // waits on its answer.
    fs::remove_file(crate::ask::host_path(&w, "question-needs-owner")).unwrap();
    fs::remove_file(b.cfg.jobs.join("j1/1.outcome")).unwrap();
    fs::write(b.path("active"), "").unwrap();
    assert_eq!(
        crate::record::derive(&w).stage,
        crate::record::Stage::Queued
    );
    let r = b.pass().look().record("sam/box", "7", None);
    assert_eq!(
        (r.stage, r.wait),
        (Stage::Working, Some(Wait::Answer("j1".into())))
    );
    fs::remove_file(b.path("active")).unwrap();
    fs::write(b.cfg.jobs.join("j1/1.outcome"), "done\n").unwrap();
    fs::write(crate::ask::host_path(&w, "question-needs-owner"), "1\n").unwrap();

    // Parked by the policy: its own park, not its repo's pause.
    crate::policy::mark(&w, "done", false, Some("a free claude slot")).unwrap();
    let jr = crate::record::derive(&w);
    assert_eq!(jr.stage, crate::record::Stage::Parked, "{jr:?}");
    fs::remove_file(crate::ask::host_path(&w, "question-needs-owner")).unwrap();
    fs::remove_file(crate::ask::host_path(&w, "question")).unwrap();
    fs::remove_file(b.cfg.jobs.join("j1/1.end")).unwrap();
    let r = b.pass().look().record("sam/box", "7", None);
    assert!(
        matches!(&r.wait, Some(Wait::Policy { job, ends }) if job == "j1" && ends.contains("a free claude slot")),
        "{r:?}"
    );

    // A sorting job parked the same way.
    fs::remove_file(crate::ask::host_path(&job, "question-needs-owner")).unwrap();
    fs::remove_file(crate::ask::host_path(&job, "question")).unwrap();
    fs::remove_file(b.cfg.jobs.join("s3/1.end")).unwrap();
    crate::policy::mark(&job, "done", false, Some("a free claude slot")).unwrap();
    let r = b.pass().look().record("sam/box", "3", None);
    assert_eq!(r.stage, Stage::Sorting);
    assert!(
        matches!(&r.wait, Some(Wait::Policy { job, ends }) if job == "s3" && ends.contains("a free claude slot")),
        "{r:?}"
    );
}
