//! Issues already carried by an open closing PR are skipped rather than
//! launched as duplicate jobs, split, drained, or offered as task-list
//! children, and resume normally if that PR closes without merging.

use std::collections::BTreeSet;
use std::fs;

use serde_json::{Value, json};

use super::forget;
use super::record::{Stage, Unseen, Wait, files};
use super::sorting_tests::{Box, issue};
use crate::snapshot::Snapshot;

fn set_closing_prs(b: &Box, n: u64, nodes: Value) {
    fs::write(
        b.path(&format!("merged-{n}")),
        json!({
            "data": {
                "repository": {
                    "issue": {
                        "closedByPullRequestsReferences": {
                            "nodes": nodes
                        }
                    }
                }
            }
        })
        .to_string(),
    )
    .unwrap();
}

fn open_closing_pr(b: &Box, n: u64, url: &str) {
    set_closing_prs(
        b,
        n,
        json!([{"state": "OPEN", "merged": false, "url": url}]),
    );
}

fn closed_unmerged_pr(b: &Box, n: u64, url: &str) {
    set_closing_prs(
        b,
        n,
        json!([{"state": "CLOSED", "merged": false, "url": url}]),
    );
}

fn runnable(b: &mut Box) {
    b.cfg.projects = b.path("projects");
    fs::create_dir_all(b.cfg.projects.join("box")).unwrap();
    b.cfg.tools.systemd_run = "true".into();
    b.cfg.tools.systemctl = "false".into();
}

#[test]
fn open_closing_pr_blocks_new_inbox_job_and_unblocks_when_closed_without_merging() {
    let mut b = Box::new();
    runnable(&mut b);

    let mut task = issue(7, "sam", &["claude"]);
    task["state"] = json!("open");
    fs::write(b.path("repos_sam_box_issues_7"), task.to_string()).unwrap();

    // Seed a stale drained mark without a job to verify it is cleared so no
    // drain slot is held while carried by an open PR.
    let worker = files::worker(&b.cfg.inbox.as_ref().unwrap().state, "sam_box", "7");
    fs::create_dir_all(&worker).unwrap();
    fs::write(worker.join(files::File::Drained), "sam/box#7\n").unwrap();

    let pr = "https://github.com/sam/box/pull/1494";
    open_closing_pr(&b, 7, pr);

    let mut pass = b.pass();
    pass.new_issues("sam/box", std::slice::from_ref(&task))
        .unwrap();
    assert!(
        !worker.join(files::File::Job).exists(),
        "an issue with an already-open closing PR must not start a new inbox job"
    );
    assert!(!worker.exists(), "stale drained mark should be cleared");
    assert!(pass.wake.contains_key("sam/box"));
    assert_eq!(
        files::carried(&b.cfg.inbox.as_ref().unwrap().state, "sam_box", "7").as_deref(),
        Some(pr)
    );

    // Record, `proj why`, and `proj status` all show it is carried by that PR.
    let rec = b.pass().look().record("sam/box", "7", Some(&task));
    assert_eq!(rec.stage, Stage::Unseen(Unseen::Carried(pr.into())));
    assert_eq!(rec.wait, Some(Wait::ClosingPr(pr.into())));
    let why = b.pass().why("sam/box", "7", false);
    assert!(why.contains(pr), "{why}");
    assert!(why.contains("open closing PR"), "{why}");
    let snap = Snapshot::take(&b.cfg, 10).unwrap();
    assert_eq!(
        snap.holds.carried,
        vec![format!("sam/box#7: carried by open closing PR {pr}")]
    );
    assert!(snap.status().contains(&format!(
        "carried: sam/box#7: carried by open closing PR {pr}"
    )));

    // Once the PR closes without merging and the issue remains open, the next
    // pass clears the carried state and launches the job normally.
    closed_unmerged_pr(&b, 7, pr);
    b.pass()
        .new_issues("sam/box", std::slice::from_ref(&task))
        .unwrap();
    assert!(worker.join(files::File::Job).exists());
    assert_eq!(
        files::carried(&b.cfg.inbox.as_ref().unwrap().state, "sam_box", "7"),
        None
    );
}

#[test]
fn open_closing_pr_blocks_triage_and_split_and_unblocks_when_closed() {
    let mut b = Box::new();
    runnable(&mut b);
    b.cfg.inbox.as_mut().unwrap().planning = true;

    let mut task = issue(1386, "sam", &[]);
    task["state"] = json!("open");
    fs::write(b.path("repos_sam_box_issues_1386"), task.to_string()).unwrap();

    // Pre-seed a planner wait to ensure it is cleared once carried.
    let wait_file = files::sorting_waits(&b.cfg.inbox.as_ref().unwrap().state).join("sam_box-1386");
    fs::create_dir_all(wait_file.parent().unwrap()).unwrap();
    fs::write(&wait_file, "sam/box#1386: waiting for a planner slot").unwrap();

    let pr = "https://github.com/sam/box/pull/1494";
    open_closing_pr(&b, 1386, pr);

    b.pass()
        .triage("sam/box", std::slice::from_ref(&task))
        .unwrap();
    let triage_dir = files::sorting(&b.cfg.inbox.as_ref().unwrap().state, "sam_box", "1386");
    assert!(
        !triage_dir.exists(),
        "an issue with an open closing PR must not start a sorting/planner job"
    );
    assert!(!wait_file.exists(), "planner wait should be cleared");
    assert_eq!(
        files::carried(&b.cfg.inbox.as_ref().unwrap().state, "sam_box", "1386").as_deref(),
        Some(pr)
    );

    // Even if a planner job was already running and returned a `split` verdict
    // after PR #1494 opened, carrying out the split is skipped so no duplicate
    // child issues are created.
    b.cfg.tools.systemctl = b.path("systemctl");
    let split_answer = "split high\n```json\n[{\"title\":\"child 1\",\"body\":\"one\",\"check\":\"cargo test\"},{\"title\":\"child 2\",\"body\":\"two\",\"check\":\"cargo test\"}]\n```";
    b.job("p1", "done", split_answer);
    fs::create_dir_all(&triage_dir).unwrap();
    fs::write(triage_dir.join(files::File::Job), "p1\n").unwrap();
    let input = b.pass().issue_text("sam/box", &task, &[]);
    fs::write(triage_dir.join(files::File::Input), &input).unwrap();
    b.pass()
        .triage("sam/box", std::slice::from_ref(&task))
        .unwrap();
    assert!(
        !b.log().contains("POST repos/sam/box/issues "),
        "must not create duplicate child issues while closing PR is open: {}",
        b.log()
    );
    assert!(
        !b.pass().has_task_list(Some("sam/box"), "1386"),
        "must not persist a task list while closing PR is open"
    );
    assert!(!triage_dir.exists(), "stopped planner record should clear");

    // Once PR #1494 closes without merging, the next triage pass picks #1386 up.
    b.cfg.tools.systemctl = "false".into();
    closed_unmerged_pr(&b, 1386, pr);
    b.pass()
        .triage("sam/box", std::slice::from_ref(&task))
        .unwrap();
    assert!(
        triage_dir.join(files::File::Job).exists(),
        "after closing PR closes unmerged, triage should start normally"
    );
    assert_eq!(
        files::carried(&b.cfg.inbox.as_ref().unwrap().state, "sam_box", "1386"),
        None
    );
}

#[test]
fn open_closing_pr_skips_in_drain_without_blocking_other_ready_issues() {
    let mut b = Box::new();
    runnable(&mut b);

    let mut carried = issue(7, "sam", &["ready", "priority: high"]);
    carried["state"] = json!("open");
    carried["created_at"] = json!("2026-11-01T00:00:00Z");
    carried["repository_url"] = json!("https://api.github.com/repos/sam/box");

    let mut other = issue(8, "sam", &["ready", "priority: medium"]);
    other["state"] = json!("open");
    other["created_at"] = json!("2026-11-02T00:00:00Z");
    other["repository_url"] = json!("https://api.github.com/repos/sam/box");

    fs::write(b.path("repos_sam_box_issues_7"), carried.to_string()).unwrap();
    fs::write(b.path("repos_sam_box_issues_8"), other.to_string()).unwrap();

    let pr = "https://github.com/sam/box/pull/1494";
    open_closing_pr(&b, 7, pr);

    b.pass()
        .note_ready("sam/box", &[carried.clone(), other.clone()])
        .unwrap();
    b.pass().drain().unwrap();

    assert!(
        !b.log().contains("repos/sam/box/issues/7/labels"),
        "carried issue #7 must not be labelled by the drain: {}",
        b.log()
    );
    assert!(
        b.log()
            .contains("POST repos/sam/box/issues/8/labels -f labels[]=claude"),
        "drain should continue past carried #7 and hand on #8: {}",
        b.log()
    );

    // Snapshot queue omits carried #7 and reports it under holds.carried.
    let snap = Snapshot::take(&b.cfg, 10).unwrap();
    assert!(
        !snap
            .queue
            .iter()
            .any(|r| r.repo == "sam/box" && r.number == 7),
        "carried issue should not sit in ready queue: {:?}",
        snap.queue
    );
    assert_eq!(
        snap.holds.carried,
        vec![format!("sam/box#7: carried by open closing PR {pr}")]
    );

    // Remove #8's drained record (simulating #8 completing) and close PR #1494
    // without merging; reconciling carried PRs unblocks #7 for the next drain.
    let _ = fs::remove_dir_all(files::worker(
        &b.cfg.inbox.as_ref().unwrap().state,
        "sam_box",
        "8",
    ));
    closed_unmerged_pr(&b, 7, pr);
    b.pass().reconcile_carried_prs("sam/box", None).unwrap();
    assert_eq!(
        files::carried(&b.cfg.inbox.as_ref().unwrap().state, "sam_box", "7"),
        None
    );
    b.pass().drain().unwrap();
    assert!(
        b.log()
            .contains("POST repos/sam/box/issues/7/labels -f labels[]=claude"),
        "after closing PR closes unmerged, drain should pick up #7: {}",
        b.log()
    );
}

#[test]
fn open_closing_pr_holds_task_list_child_and_resumes_when_closed() {
    let mut b = Box::new();
    runnable(&mut b);
    b.cfg.inbox.as_mut().unwrap().planning = true;

    let mut parent = issue(7, "sam", &["tracker"]);
    parent["state"] = json!("open");
    fs::write(b.path("repos_sam_box_issues_7"), parent.to_string()).unwrap();
    let p = b.pass();
    let parent_input = p.issue_text("sam/box", &parent, &[]);
    p.keep_planned("sam/box", "7", &parent_input).unwrap();
    let plan_dir = b.cfg.inbox.as_ref().unwrap().state.join(".plans/sam_box-7");
    let tasks_json = json!({
        "source": null,
        "revision": 0,
        "retained": [],
        "tasks": [{"issue": null, "carries": null, "title": "step 1", "body": "do it", "check": "cargo test"}],
        "children": [{"number": 8, "attempted": true, "posting_id": 42, "merged_pr": null, "offered": false, "marker": null, "waits": false, "continued": null}],
        "comment": 100,
        "rendered": "Task list for sam/box#7. Each implementation child is one fresh session and one PR.\n\n- [ ] #8: step 1"
    });
    fs::write(plan_dir.join("tasks.json"), tasks_json.to_string()).unwrap();

    let mut child = issue(8, "sam", &["later"]);
    child["state"] = json!("open");
    fs::write(b.path("repos_sam_box_issues_8"), child.to_string()).unwrap();

    let pr = "https://github.com/sam/box/pull/1494";
    open_closing_pr(&b, 8, pr);

    b.pass().sequence("sam/box", std::slice::from_ref(&parent));
    assert!(
        !b.log().contains("repos/sam/box/issues/8/labels"),
        "task-list child with open closing PR must not be offered as ready: {}",
        b.log()
    );
    let held = fs::read_to_string(plan_dir.join("held")).unwrap();
    assert!(held.contains("#8") && held.contains(pr), "{held}");
    assert_eq!(
        files::carried(&b.cfg.inbox.as_ref().unwrap().state, "sam_box", "8").as_deref(),
        Some(pr)
    );

    // Once PR #1494 closes without merging, next sequence offers child #8.
    closed_unmerged_pr(&b, 8, pr);
    b.pass().sequence("sam/box", std::slice::from_ref(&parent));
    assert!(
        b.log()
            .contains("POST repos/sam/box/issues/8/labels -f labels[]=ready"),
        "child should be offered once closing PR closes unmerged: {}",
        b.log()
    );
    assert!(!plan_dir.join("held").exists());
    assert_eq!(
        files::carried(&b.cfg.inbox.as_ref().unwrap().state, "sam_box", "8"),
        None
    );
}

#[test]
fn refs_only_open_pr_does_not_block_starting_issue() {
    let mut b = Box::new();
    runnable(&mut b);

    let mut task = issue(7, "sam", &["claude"]);
    task["state"] = json!("open");
    fs::write(b.path("repos_sam_box_issues_7"), task.to_string()).unwrap();

    // An unrelated open PR #20 mentions `Refs #7` (cross-referenced, NOT a
    // closing PR): `closedByPullRequestsReferences` has no OPEN closing PRs
    // (only an empty list, or closed/merged history).
    fs::write(
        b.path("repos_sam_box_pulls"),
        json!([{"number": 20, "state": "open", "body": "Refs #7", "html_url": "https://github.com/sam/box/pull/20"}]).to_string(),
    )
    .unwrap();
    set_closing_prs(b_ref(&b), 7, json!([]));

    b.pass()
        .new_issues("sam/box", std::slice::from_ref(&task))
        .unwrap();
    assert!(
        files::worker(&b.cfg.inbox.as_ref().unwrap().state, "sam_box", "7")
            .join(files::File::Job)
            .exists(),
        "an open PR that only mentions `Refs #7` must not block starting the issue"
    );
    assert_eq!(
        files::carried(&b.cfg.inbox.as_ref().unwrap().state, "sam_box", "7"),
        None
    );
    // Verify a merged/closed historical closing PR doesn't block either.
    let mut task2 = issue(9, "sam", &["claude"]);
    task2["state"] = json!("open");
    fs::write(b.path("repos_sam_box_issues_9"), task2.to_string()).unwrap();
    set_closing_prs(
        &b,
        9,
        json!([
            {"state": "CLOSED", "merged": false, "url": "https://github.com/sam/box/pull/19"},
            {"state": "MERGED", "merged": true, "url": "https://github.com/sam/box/pull/18"}
        ]),
    );
    b.pass().new_issues("sam/box", &[task2]).unwrap();
    assert!(
        files::worker(&b.cfg.inbox.as_ref().unwrap().state, "sam_box", "9")
            .join(files::File::Job)
            .exists(),
        "non-OPEN closing PRs must not block starting the issue"
    );
}

fn b_ref(b: &Box) -> &Box {
    b
}

#[test]
fn unavailable_closing_pr_lookup_fails_closed() {
    let mut b = Box::new();
    runnable(&mut b);

    let mut task = issue(7, "sam", &["claude"]);
    task["state"] = json!("open");
    fs::write(b.path("repos_sam_box_issues_7"), task.to_string()).unwrap();
    fs::write(b.path("fail"), "merged-query\n").unwrap();

    let mut pass = b.pass();
    pass.new_issues("sam/box", std::slice::from_ref(&task))
        .unwrap();
    assert!(
        !files::worker(&b.cfg.inbox.as_ref().unwrap().state, "sam_box", "7")
            .join(files::File::Job)
            .exists(),
        "must fail closed when closing-PR lookup fails"
    );
    assert!(pass.unreached.contains(&"sam/box".to_owned()));
    assert!(pass.wake.contains_key("sam/box"));

    let mut unsorted = issue(8, "sam", &[]);
    unsorted["state"] = json!("open");
    fs::write(b.path("repos_sam_box_issues_8"), unsorted.to_string()).unwrap();
    let mut pass = b.pass();
    pass.triage("sam/box", &[unsorted]).unwrap();
    assert!(
        !files::sorting(&b.cfg.inbox.as_ref().unwrap().state, "sam_box", "8").exists(),
        "triage must fail closed when closing-PR lookup fails"
    );
    assert!(pass.unreached.contains(&"sam/box".to_owned()));
}

#[test]
fn forget_and_reconcile_clean_up_carried_records() {
    let b = Box::new();
    let state = &b.cfg.inbox.as_ref().unwrap().state;
    let carried_dir = files::carried_root(state);
    fs::create_dir_all(&carried_dir).unwrap();
    fs::write(
        carried_dir.join("sam_box-7"),
        "sam/box#7\thttps://github.com/sam/box/pull/10\n",
    )
    .unwrap();
    fs::write(
        carried_dir.join("sam_box-8"),
        "sam/box#8\thttps://github.com/sam/box/pull/11\n",
    )
    .unwrap();
    fs::write(
        carried_dir.join("sam_boxx-7"),
        "sam/boxx#7\thttps://github.com/sam/boxx/pull/12\n",
    )
    .unwrap();
    fs::write(
        carried_dir.join("sam_gone-1"),
        "sam/gone#1\thttps://github.com/sam/gone/pull/1\n",
    )
    .unwrap();

    // Scoped reconcile leaves out-of-scope #8 untouched while retiring closed #7.
    let mut pass = b.pass();
    pass.scope = Some(BTreeSet::from([7]));
    pass.reconcile_carried_closed("sam/box", &[]).unwrap();
    assert!(!carried_dir.join("sam_box-7").exists());
    assert!(carried_dir.join("sam_box-8").exists());

    // Prune drops repos no longer in `project` (`sam/gone` and `sam/boxx`).
    // Restore `sam_boxx-7` afterwards to test `forget`'s exact repo boundary.
    b.pass().prune_carried_prs().unwrap();
    assert!(!carried_dir.join("sam_gone-1").exists());
    assert!(carried_dir.join("sam_box-8").exists());

    fs::write(
        carried_dir.join("sam_boxx-7"),
        "sam/boxx#7\thttps://github.com/sam/boxx/pull/12\n",
    )
    .unwrap();
    forget(&b.cfg, state, "SAM/BOX").unwrap();
    assert!(!carried_dir.join("sam_box-8").exists());
    assert!(carried_dir.join("sam_boxx-7").exists());
}
