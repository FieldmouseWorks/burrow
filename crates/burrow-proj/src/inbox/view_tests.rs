//! The inbox's part of the live snapshot, against the stand-ins in
//! [`super::sorting_tests`].

use std::fs;

use serde_json::Value;

use super::sorting_tests::Box;
use crate::engine::Engine;
use crate::now;
use crate::snapshot::{SlotKind, SlotState, Snapshot};

/// Job `id`, about sam/game#N, its one turn ended on `proj await` of
/// sam/game#AWAITED `ago` seconds back, on Codex's `small` at `high`; and the
/// drain's record of having handed it on.
fn parked(b: &Box, id: &str, n: u64, awaited: u64, ago: i64) {
    let d = b.cfg.jobs.join(id);
    fs::create_dir_all(&d).unwrap();
    let since = now() - ago;
    for (f, v) in [
        ("turns", "1".to_owned()),
        ("prompt.1", "the task".to_owned()),
        ("project", "/src/game".to_owned()),
        ("link", format!("https://github.com/sam/game/issues/{n}")),
        ("role", "inbox".to_owned()),
        ("1.engine", "codex".to_owned()),
        ("1.model", "small".to_owned()),
        ("1.effort", "high".to_owned()),
        ("1.outcome", "done".to_owned()),
        ("1.end", "await".to_owned()),
        (
            "1.result",
            format!("Opened sam/game#{awaited}; waiting on it."),
        ),
        ("await", format!("sam/game#{awaited}\t{since}\tfp\n")),
    ] {
        fs::write(d.join(f), format!("{v}\n")).unwrap();
    }
    crate::policy::bind(
        &crate::job::Job::new(&b.cfg, id),
        crate::policy::Identity::Repository("sam/game".into()),
        crate::policy::Kind::Automated,
    )
    .unwrap();
    let r = b
        .cfg
        .inbox
        .as_ref()
        .unwrap()
        .state
        .join(format!("sam_game-{n}"));
    fs::create_dir_all(&r).unwrap();
    fs::write(r.join("drained"), format!("sam/game#{n}\n")).unwrap();
    fs::write(r.join("job"), format!("{id}\n")).unwrap();
}

/// 2026-11-11's stall: three jobs the drain handed on, each parked on an
/// await, held all three drain slots. They don't now, and the snapshot says
/// so: every slot is idle and the jobs show what they await. `proj status`
/// prints the same snapshot.
#[test]
fn the_11_11_stall() {
    let mut b = Box::new();
    b.cfg.projects = b.path("src");
    let t = b.cfg.inbox.as_mut().unwrap().triage.as_mut().unwrap();
    t.drain = Some(Engine::Codex);
    t.drain_jobs = 3;
    for (i, n) in [11u64, 12, 13].into_iter().enumerate() {
        parked(&b, &format!("job-{n}"), n, 100 + n, 600 * (i as i64 + 1));
    }
    let json = crate::cmd::state_json(&b.cfg, 15).unwrap();
    let json: Value = serde_json::from_str(&json).unwrap();
    crate::snapshot::tests::validates(&json).unwrap();
    let snap: Snapshot = serde_json::from_value(json).unwrap();
    let drain: Vec<_> = snap
        .slots
        .iter()
        .filter(|s| s.kind == SlotKind::Drain)
        .collect();
    assert_eq!(drain.len(), 3, "{drain:#?}");
    // A job parked on `proj await` holds no drain slot: all three are free,
    // and the jobs show what each waits on, and since when.
    assert!(
        drain
            .iter()
            .all(|s| s.state == SlotState::Idle && s.holder.is_none()),
        "{drain:#?}"
    );
    let mut seen = Vec::new();
    for j in &snap.jobs {
        let n: u64 = j.id.trim_start_matches("job-").parse().unwrap();
        assert_eq!(j.awaits, [format!("sam/game#{}", 100 + n)]);
        seen.push(n);
    }
    seen.sort();
    assert_eq!(seen, [11, 12, 13]);
    // None of the turn slots is held: the stall is the drain's alone.
    assert!(
        snap.slots
            .iter()
            .filter(|s| s.kind != SlotKind::Drain)
            .all(|s| s.state == SlotState::Idle)
    );
    assert_eq!(
        snap.holds.triage.as_ref().map(|t| (t.drain, t.drain_jobs)),
        Some((Some(Engine::Codex), 3))
    );

    // `proj status` is the snapshot's, line for line: each job as the
    // snapshot has it, newest first.
    let status = snap.status();
    let rows: Vec<(String, String)> = status
        .lines()
        .filter(|l| !l.starts_with("drain:"))
        .map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            (f[0].to_owned(), f[1].to_owned())
        })
        .collect();
    let jobs: Vec<(String, String)> = snap
        .jobs
        .iter()
        .map(|j| (j.id.clone(), j.state.clone()))
        .collect();
    assert_eq!(rows, jobs);
    assert_eq!(rows.len(), 3);
    for j in &snap.jobs {
        let line = status.lines().find(|l| l.starts_with(&j.id)).unwrap();
        assert!(
            line.contains(&format!("[awaits {}] ", j.awaits[0])),
            "{line}"
        );
        assert!(line.trim_end().ends_with("waiting on it."), "{line}");
    }
    assert_eq!(
        Snapshot::take(&b.cfg, 15).unwrap().status(),
        status,
        "proj status prints the snapshot"
    );
}

// A sorting job parked on `proj await` still takes one of the `atOnce`
// sorting slots: the snapshot shows that slot parked, what holds it, on
// what, and for how long.
#[test]
fn a_parked_sorting_job_holds_its_slot() {
    let mut b = Box::new();
    b.cfg.projects = b.path("src");
    parked(&b, "sort-21", 21, 121, 900);
    let state = &b.cfg.inbox.as_ref().unwrap().state;
    // Its record is the sorting pass's, not the drain's.
    fs::remove_dir_all(state.join("sam_game-21")).unwrap();
    let r = state.join(".triage").join("sam_game-21");
    fs::create_dir_all(&r).unwrap();
    fs::write(r.join("job"), "sort-21\n").unwrap();
    let snap = Snapshot::take(&b.cfg, 15).unwrap();
    let sorting: Vec<_> = snap
        .slots
        .iter()
        .filter(|s| s.kind == SlotKind::Sorting)
        .collect();
    assert_eq!(sorting.len(), 2, "{sorting:#?}");
    let s = sorting[0];
    assert_eq!(s.state, SlotState::Parked, "{s:#?}");
    let h = s.holder.as_ref().unwrap();
    assert_eq!(h.job.as_deref(), Some("sort-21"));
    assert_eq!(h.awaits, ["sam/game#121"]);
    assert_eq!(
        (h.engine, h.model.as_deref(), h.effort.as_deref()),
        (Some(Engine::Codex), Some("small"), Some("high"))
    );
    let age = s.age.unwrap();
    assert!((900..905).contains(&age), "{age}s, not 900s");
    assert_eq!(s.since.unwrap() + age, snap.at);
    assert_eq!(sorting[1].state, SlotState::Idle);
}

#[test]
fn status_shows_every_drain_handoff_beyond_the_job_limit() {
    let mut b = Box::new();
    b.cfg.projects = b.path("src");
    let t = b.cfg.inbox.as_mut().unwrap().triage.as_mut().unwrap();
    t.drain = Some(Engine::Codex);
    t.drain_jobs = 2;
    fs::write(b.path("active"), "").unwrap();
    for count in 0..=3 {
        if count > 0 {
            let r = b
                .cfg
                .inbox
                .as_ref()
                .unwrap()
                .state
                .join(format!("sam_game-{count}"));
            fs::create_dir_all(&r).unwrap();
            fs::write(r.join("drained"), format!("sam/game#{count}")).unwrap();
            if count == 2 {
                let job = b.cfg.jobs.join("old-job");
                fs::create_dir_all(&job).unwrap();
                fs::write(job.join("prompt.1"), "task").unwrap();
                fs::write(job.join("turns"), "1").unwrap();
                crate::policy::bind(
                    &crate::job::Job::new(&b.cfg, "old-job"),
                    crate::policy::Identity::Repository("sam/game".into()),
                    crate::policy::Kind::Automated,
                )
                .unwrap();
                fs::write(r.join("job"), "old-job").unwrap();
            }
        }
        let status = Snapshot::take(&b.cfg, 0).unwrap().status();
        assert!(
            status.contains(&format!("drain: {count}/2 used\n")),
            "{status}"
        );
        for n in 1..=count {
            let suffix = if n == 2 {
                "old-job: queued"
            } else {
                "enrolling"
            };
            assert!(
                status.contains(&format!("  sam/game#{n} ({suffix})\n")),
                "{status}"
            );
        }
    }
    fs::write(b.cfg.jobs.join("old-job/1.slot"), "1").unwrap();
    let status = Snapshot::take(&b.cfg, 0).unwrap().status();
    assert!(status.contains("sam/game#2 (old-job: running)"), "{status}");
    b.cfg
        .inbox
        .as_mut()
        .unwrap()
        .triage
        .as_mut()
        .unwrap()
        .drain_jobs = 0;
    let status = Snapshot::take(&b.cfg, 0).unwrap().status();
    assert!(status.contains("drain: 3/0 used"), "{status}");
    b.cfg.inbox.as_mut().unwrap().triage.as_mut().unwrap().drain = None;
    assert!(
        !Snapshot::take(&b.cfg, 0)
            .unwrap()
            .status()
            .contains("drain:")
    );
}
