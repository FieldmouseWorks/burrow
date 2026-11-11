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
/// await, held all three drain slots, and nothing said so. The snapshot
/// shows each slot parked, what holds it, on what, and for how long, and
/// `proj status` prints the same snapshot.
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
    let mut seen = Vec::new();
    for s in &drain {
        assert_eq!(s.state, SlotState::Parked, "{s:#?}");
        let h = s.holder.as_ref().unwrap();
        let job = h.job.clone().unwrap();
        let n: u64 = job.trim_start_matches("job-").parse().unwrap();
        assert_eq!(h.awaits, [format!("sam/game#{}", 100 + n)]);
        assert_eq!(
            h.issue.as_deref(),
            Some(format!("https://github.com/sam/game/issues/{n}").as_str())
        );
        assert_eq!(
            (
                h.engine,
                h.model.as_deref(),
                h.effort.as_deref(),
                h.role.as_deref()
            ),
            (
                Some(Engine::Codex),
                Some("small"),
                Some("high"),
                Some("inbox")
            )
        );
        // Parked since its await, for as long as it has waited.
        let want = 600 * (n as i64 - 10);
        let age = s.age.unwrap();
        assert!(
            (want..want + 5).contains(&age),
            "{job}: {age}s, not {want}s"
        );
        assert_eq!(s.since.unwrap() + age, snap.at);
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
