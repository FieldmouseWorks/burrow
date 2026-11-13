//! Admission and wait records across terminal turns, holds and issue discovery.
use super::{Box, issue};
use crate::{inbox, job::Job};
use serde_json::json;
use std::{fs, path::PathBuf};

fn single_slot() -> Box {
    let mut b = Box::new();
    b.cfg
        .inbox
        .as_mut()
        .unwrap()
        .triage
        .as_mut()
        .unwrap()
        .at_once = 1;
    b
}

fn record(b: &Box, repo: &str, n: u64, id: &str) -> PathBuf {
    let d = b
        .cfg
        .inbox
        .as_ref()
        .unwrap()
        .state
        .join(".triage")
        .join(format!("{}-{n}", inbox::key(repo)));
    fs::create_dir_all(&d).unwrap();
    fs::write(d.join("job"), id).unwrap();
    d
}

fn wait(b: &Box, repo: &str, n: &str) -> PathBuf {
    b.cfg
        .inbox
        .as_ref()
        .unwrap()
        .state
        .join(".triage-waiting")
        .join(format!("{}-{n}", inbox::key(repo)))
}

fn seed_wait(b: &Box, repo: &str, n: &str) -> PathBuf {
    let f = wait(b, repo, n);
    fs::create_dir_all(f.parent().unwrap()).unwrap();
    fs::write(&f, format!("{repo}#{n}: waiting for a planner slot")).unwrap();
    f
}

// Exercise the shared predicate through every consumer, including a different
// repo's planning when the first repo's verdict cannot be reconciled.
fn decisions(b: &Box, expected: bool) {
    let mut p = b.pass();
    p.triage("sam/box", &[issue(9, "sam", &[])]).unwrap();
    assert_eq!(wait(b, "sam/box", "9").exists(), expected, "planning");
    assert_eq!(p.wake.is_empty(), expected, "admission attempt");
    let view = inbox::view::view(&b.cfg, 15);
    assert_eq!(
        view.slots
            .iter()
            .any(|s| s.kind == crate::snapshot::SlotKind::Sorting && s.holder.is_some()),
        expected,
        "snapshot"
    );
    b.job("candidate", "done", "kept");
    record(b, "sam/box", 88, "candidate");
    assert_eq!(
        matches!(
            inbox::resumption::admission(&b.cfg, &Job::new(&b.cfg, "candidate")).unwrap(),
            inbox::resumption::Resume::Waiting
        ),
        expected,
        "resume admission"
    );
}

#[test]
fn terminal_sorters_release_capacity_without_requiring_an_outcome() {
    for terminal in ["done", "error", "unit-kill", "reboot", "stopped"] {
        let b = single_slot();
        b.job("s1", "done", "kept verdict");
        let job = Job::new(&b.cfg, "s1");
        match terminal {
            "error" => fs::write(job.turn("1", "outcome"), "error").unwrap(),
            "unit-kill" | "reboot" | "stopped" => {
                fs::remove_file(job.turn("1", "outcome")).unwrap();
                if terminal == "unit-kill" {
                    job.mark_ended();
                }
                if terminal == "stopped" {
                    fs::write(job.turn("1", "stopped"), "").unwrap();
                }
            }
            _ => (),
        }
        let d = record(&b, "sam/game", 3, "s1");
        assert!(!b.pass().sorting_slot(&d), "{terminal}");
        decisions(&b, false);
    }
}

#[test]
fn live_retry_and_outstanding_holds_keep_sorting_capacity() {
    for hold in [
        "queued", "running", "retry", "ask", "question", "await", "owner",
    ] {
        let b = single_slot();
        b.job("s1", "done", "kept verdict");
        let job = Job::new(&b.cfg, "s1");
        match hold {
            "queued" | "running" => {
                fs::remove_file(job.turn("1", "outcome")).unwrap();
                fs::write(b.path("active"), "").unwrap();
                if hold == "running" {
                    fs::write(job.turn("1", "slot"), "1").unwrap();
                }
            }
            "retry" => {
                fs::remove_file(job.turn("1", "outcome")).unwrap();
                fs::write(job.turn("1", "retry-migration"), "pending").unwrap();
            }
            "ask" => fs::write(job.turn("1", "end"), "ask").unwrap(),
            "question" => {
                fs::write(crate::ask::host_path(&job, "question"), "unreadable json").unwrap()
            }
            "await" => {
                fs::write(job.path("await"), "https://github.com/sam/game/pull/4\n").unwrap()
            }
            "owner" => fs::write(crate::ask::host_path(&job, "question-needs-owner"), "").unwrap(),
            _ => unreachable!(),
        }
        record(&b, "sam/game", 3, "s1");
        decisions(&b, hold != "owner");
    }
}

#[test]
fn unreadable_holds_reserve_capacity_for_every_consumer() {
    for metadata in ["question", "end", "await"] {
        for owner in [false, true] {
            let b = single_slot();
            b.job("s1", "done", "kept verdict");
            let job = Job::new(&b.cfg, "s1");
            let path = match metadata {
                "question" => crate::ask::host_path(&job, "question"),
                "end" => {
                    fs::remove_file(job.turn("1", "end")).unwrap();
                    job.turn("1", "end")
                }
                _ => job.path("await"),
            };
            fs::create_dir(&path).unwrap();
            if owner {
                fs::write(crate::ask::host_path(&job, "question-needs-owner"), "").unwrap();
            }
            record(&b, "sam/game", 3, "s1");
            decisions(&b, true);
        }
    }
}

#[test]
fn existing_sorting_records_are_not_waiting_for_new_admission() {
    for existing in [
        "queued",
        "running",
        "failed",
        "verdict-free",
        "verdict-busy",
    ] {
        let b = single_slot();
        b.job("s1", "done", "kept verdict");
        let d = record(&b, "sam/box", 7, "s1");
        match existing {
            "queued" | "running" => {
                fs::write(b.path("active"), "").unwrap();
                if existing == "running" {
                    fs::write(b.cfg.jobs.join("s1/1.slot"), "1").unwrap();
                }
            }
            "failed" => {
                fs::remove_file(d.join("job")).unwrap();
                fs::write(d.join("failed"), super::UPDATED).unwrap();
            }
            "verdict-free" | "verdict-busy" => {
                fs::write(d.join("plan"), "[]").unwrap();
                if existing == "verdict-busy" {
                    // Another sorter fills the lane while this verdict is pending.
                    b.job("s2", "done", "kept");
                    record(&b, "sam/game", 3, "s2");
                    fs::write(b.cfg.jobs.join("s2/1.end"), "ask").unwrap();
                }
            }
            _ => unreachable!(),
        }
        // Prevent verdict reconciliation from retiring these records.
        fs::write(b.path("fail"), "api graphql").unwrap();
        let f = seed_wait(&b, "sam/box", "7");
        b.pass().triage("sam/box", &[issue(7, "sam", &[])]).unwrap();
        assert!(d.exists(), "{existing}");
        if existing.starts_with("verdict-") {
            let why = fs::read_to_string(f).unwrap();
            assert!(why.contains("verdict's steps"), "{existing}: {why}");
            assert!(!why.contains("planner slot"), "{existing}: {why}");
        } else {
            assert!(!f.exists(), "{existing}");
        }
    }
}

#[test]
fn free_capacity_clears_wait_even_when_replacement_cannot_start() {
    for failed in [false, true] {
        let b = single_slot();
        if failed {
            let d = b
                .cfg
                .inbox
                .as_ref()
                .unwrap()
                .state
                .join(".triage/sam_box-9");
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("failed"), "older issue version").unwrap();
        }
        let f = seed_wait(&b, "sam/box", "9");
        let next = seed_wait(&b, "sam/box", "10");
        let mut p = b.pass();
        p.triage("sam/box", &[issue(9, "sam", &[]), issue(10, "sam", &[])])
            .unwrap();
        assert!(p.wake.contains_key("sam/box"), "failed launch retries");
        assert!(!f.exists());
        assert!(!next.exists());
    }
}

#[test]
fn successful_lists_remove_closed_disappeared_and_transferred_waits() {
    for remaining in [vec![], vec![issue(8, "sam", &["later"])]] {
        let b = single_slot();
        let gone = seed_wait(&b, "sam/box", "7");
        let other = seed_wait(&b, "sam/box-other", "7");
        b.pass().triage("sam/box", &remaining).unwrap();
        assert!(!gone.exists());
        assert!(other.exists());
    }
}

#[test]
fn failed_github_lists_preserve_waits_and_removed_projects_drop_only_their_own() {
    let b = single_slot();
    let kept = seed_wait(&b, "sam/box", "7");
    let removed = seed_wait(&b, "sam/gone", "7");
    fs::write(b.path("fail"), "repos/sam/box/issues?").unwrap();
    b.pass().run("sam/box").unwrap();
    assert!(kept.exists(), "an outage says nothing about open issues");
    assert!(!removed.exists());
    fs::write(b.path("fail"), "").unwrap();
    fs::write(b.path("repos_sam_box_issues"), "[]").unwrap();
    b.pass().run("sam/box").unwrap();
    assert!(!kept.exists(), "successful listing is authoritative");
}

#[test]
fn forgetting_waits_is_case_insensitive_and_preserves_other_repositories() {
    let b = single_slot();
    let gone = seed_wait(&b, "SAM/Box", "7");
    let kept = seed_wait(&b, "sam/box-other", "7");
    inbox::forget(&b.cfg, &b.cfg.inbox.as_ref().unwrap().state, "sam/box").unwrap();
    assert!(!gone.exists());
    assert!(kept.exists());
}

// One repo's verdict that can't be carried out holds no sorting slot once its
// job has ended: another repo's issue is still sorted (a job tried, which
// can't start here and books a retry). A slot held by a live job leaves the
// issue waiting, said once in the journal's record and in the snapshot.
#[test]
fn an_unfinished_verdict_does_not_hold_other_repos_slots() {
    let mut b = Box::new();
    b.cfg
        .inbox
        .as_mut()
        .unwrap()
        .triage
        .as_mut()
        .unwrap()
        .at_once = 1;
    b.job("s1", "done", "Hmm.");
    let state = b.cfg.inbox.as_ref().unwrap().state.clone();
    let game = state.join(".triage/sam_game-3");
    fs::create_dir_all(&game).unwrap();
    fs::write(game.join("job"), "s1\n").unwrap();
    fs::write(game.join("plan"), "[]").unwrap();
    let mut p = b.pass();
    p.triage("sam/box", &[issue(9, "sam", &[])]).unwrap();
    assert!(p.wake.contains_key("sam/box"), "{:?}", p.wake);
    assert!(!state.join(".triage-waiting/sam_box-9").exists());

    // The same record with a running job does hold it.
    fs::write(b.path("active"), "").unwrap();
    fs::write(b.cfg.jobs.join("s1/1.slot"), "1").unwrap();
    fs::remove_file(b.cfg.jobs.join("s1/1.outcome")).unwrap();
    let mut p = b.pass();
    p.triage("sam/box", &[issue(9, "sam", &[])]).unwrap();
    assert!(p.wake.is_empty(), "{:?}", p.wake);
    let said = fs::read_to_string(state.join(".triage-waiting/sam_box-9")).unwrap();
    assert!(
        said.contains("sam/box#9") && said.contains("planner slot"),
        "{said}"
    );
    let snap = crate::snapshot::Snapshot::take(&b.cfg, 15).unwrap();
    assert_eq!(
        serde_json::to_value(snap).unwrap()["holds"]["triage"]["waiting"],
        json!([said])
    );
}

// A fresh fixture for each consumer prevents one observation from publishing
// the hold before another consumer exercises the same transition.
fn consumer(b: &Box, which: &str, expected: bool) {
    match which {
        "planning" => {
            b.pass().triage("sam/box", &[issue(9, "sam", &[])]).unwrap();
            assert_eq!(wait(b, "sam/box", "9").exists(), expected, "{which}");
        }
        "resume" => {
            b.job("candidate", "done", "kept");
            record(b, "sam/box", 88, "candidate");
            assert_eq!(
                matches!(
                    inbox::resumption::admission(&b.cfg, &Job::new(&b.cfg, "candidate")).unwrap(),
                    inbox::resumption::Resume::Waiting
                ),
                expected,
                "{which}"
            );
        }
        "snapshot" => {
            let view = inbox::view::view(&b.cfg, 15);
            assert_eq!(
                view.slots
                    .iter()
                    .any(|s| s.kind == crate::snapshot::SlotKind::Sorting && s.holder.is_some()),
                expected,
                "{which}"
            );
        }
        _ => unreachable!(),
    }
}

#[test]
fn completion_publishes_holds_before_capacity_can_be_released() {
    for which in ["planning", "resume", "snapshot"] {
        for hold in ["question", "await", "recovered-await"] {
            let b = single_slot();
            b.job("s1", "done", "kept");
            record(&b, "sam/game", 3, "s1");
            let job = Job::new(&b.cfg, "s1");
            let (unit, path, value) = match hold {
                "question" => (job.unit(), job.turn("1", "end"), "ask"),
                "await" => (job.unit(), job.path("await"), "sam/game#4"),
                _ => (job.end_unit(), job.path("await"), "sam/game#4"),
            };
            // Finish the turn or its _ended recovery during a lifecycle query.
            // Neither turn::exec nor cmd::finish holds the launcher lock when
            // publishing these final holds.
            burrow_test::executable(
                &b.cfg.tools.systemctl,
                format!(
                    r#"#!/bin/sh
case "$*" in
  *is-active*{unit}*|*ActiveState*{unit}*)
    printf '%s' '{value}' >'{path}'
    echo inactive
    case "$*" in *is-active*) exit 3;; esac ;;
  *is-active*) exit 3 ;;
  *ActiveState*) echo inactive ;;
esac
exit 0
"#,
                    path = path.display()
                ),
            );
            consumer(&b, which, true);
        }
    }
}

#[test]
fn only_known_terminal_units_release_sorting_capacity() {
    for which in ["planning", "resume", "snapshot"] {
        for unit_kind in ["turn", "recovery"] {
            for state in [
                "active",
                "activating",
                "deactivating",
                "reloading",
                "pending",
                "query-error",
                "active-query-error",
                "spawn-error",
                "misleading-error",
                "unknown",
                "inactive",
                "failed",
            ] {
                let mut b = single_slot();
                b.job("s1", "done", "kept");
                let job = Job::new(&b.cfg, "s1");
                fs::remove_file(job.turn("1", "outcome")).unwrap();
                record(&b, "sam/game", 3, "s1");
                let unit = if unit_kind == "turn" {
                    job.unit()
                } else {
                    job.end_unit()
                };
                burrow_test::executable(
                    &b.cfg.tools.systemctl,
                    format!(
                        r#"#!/bin/sh
case "$*" in
  *{unit}*)
    case '{state}' in
      query-error) exit 1 ;;
    esac
    case "$*" in
      *is-active*) exit 3 ;;
      *Job*) if [ '{state}' = pending ]; then echo 42; fi ;;
      *ActiveState*) case '{state}' in active-query-error) exit 1;; misleading-error) echo inactive; exit 1;; pending) echo inactive;; unknown) :;; *) echo '{state}';; esac ;;
    esac ;;
  *is-active*) exit 3 ;;
  *ActiveState*) echo inactive ;;
esac
exit 0
"#
                    ),
                );
                if state == "spawn-error" {
                    b.cfg.tools.systemctl = b.path("missing-systemctl");
                }
                consumer(&b, which, !matches!(state, "inactive" | "failed"));
            }
        }
    }
}

#[test]
fn wait_updates_clear_and_deduplicate_persisted_repository_spellings() {
    let b = single_slot();
    let old = seed_wait(&b, "SAM/Box", "7");
    let duplicate = seed_wait(&b, "Sam/bOX", "7");
    let other_repo = seed_wait(&b, "sam/box-other", "7");
    let other_issue = seed_wait(&b, "SAM/Box", "8");
    let p = b.pass();
    p.planner_wait("sam/box", "7", Some("verdict's steps"));
    let current = wait(&b, "sam/box", "7");
    assert_eq!(
        fs::read_to_string(&current).unwrap(),
        "sam/box#7: verdict's steps"
    );
    assert!(!old.exists());
    assert!(!duplicate.exists());
    assert!(other_repo.exists());
    assert!(other_issue.exists());
    p.planner_wait("sAm/BoX", "7", None);
    assert!(!current.exists());
    assert!(other_repo.exists());
    assert!(other_issue.exists());
    let old = seed_wait(&b, "SAM/Box", "7");
    b.pass()
        .triage("sam/box", &[issue(7, "sam", &["later"])])
        .unwrap();
    assert!(!old.exists());
    assert!(other_repo.exists());
}
