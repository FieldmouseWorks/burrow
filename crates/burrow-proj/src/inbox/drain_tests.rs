//! The drain: which ready issue it hands on, when there's room for it, and
//! that nothing one repo does holds up the rest, against the stand-ins in
//! [`super::sorting_tests`].

use std::fs;

use serde_json::{Value, json};

use super::sorting_tests::{Box, issue, on_github};
use crate::engine::Engine;

fn found(repo: &str, n: u64, opener: &str, labels: &[&str], made: &str) -> Value {
    let mut i = issue(n, opener, labels);
    i["repository_url"] = json!(format!("https://api.github.com/repos/{repo}"));
    i["created_at"] = json!(made);
    i
}

// The drain hands on a project's ready issue opened by an author, nobody
// else's already: the pipeline's repos first, then the oldest.
#[test]
fn the_drain_picks() {
    let b = Box::new();
    let p = b.pass();
    let t = b.cfg.inbox.as_ref().unwrap().triage.clone().unwrap();
    let old = "2026-10-01T00:00:00Z";
    let new = "2026-11-01T00:00:00Z";
    let pick = |items: &[Value]| p.pick_ready(&t, items);

    let game = found("sam/game", 4, "sam", &["ready"], old);
    let game_newer = found("sam/game", 5, "sam", &["ready"], new);
    let box_new = found("sam/box", 8, "sam", &["ready"], new);
    // sam/burrow and sam/box are pipeline repos; sam/game isn't.
    let burrow_new = found("sam/burrow", 9, "sam", &["ready"], new);
    assert_eq!(
        pick(&[game_newer.clone(), game.clone()]),
        Some(("sam/game".into(), "4".into()))
    );
    assert_eq!(
        pick(&[game.clone(), box_new.clone()]),
        Some(("sam/box".into(), "8".into()))
    );
    assert_eq!(
        pick(&[game.clone(), burrow_new.clone(), box_new.clone()]),
        Some(("sam/box".into(), "8".into()))
    );

    for passed_over in [
        found("sam/game", 4, "player", &["ready"], old),
        found("sam/game", 4, "sam", &["ready", "codex"], old),
        found("sam/game", 4, "sam", &["ready", "needs-owner"], old),
        found("sam/game", 4, "sam", &["ready", "probe"], old),
        found("sam/elsewhere", 4, "sam", &["ready"], old),
        found("sam/game", 4, "sam", &["later"], old),
        found("sam/game", 4, "sam", &["ready", "outside text"], old),
    ] {
        assert_eq!(
            pick(std::slice::from_ref(&passed_over)),
            None,
            "{passed_over}"
        );
    }
    // One with a record already (a job, or one refused) is the inbox's.
    fs::create_dir_all(b.cfg.inbox.as_ref().unwrap().state.join("sam_game-4")).unwrap();
    assert_eq!(
        pick(&[game, game_newer]),
        Some(("sam/game".into(), "5".into()))
    );
}

// Room for the drain: a Codex slot free; for Claude, a slot free beyond the
// pipeline's and nobody in line.
#[test]
fn room_for_the_drain() {
    let b = Box::new();
    let cfg = &b.cfg;
    assert!(crate::slot::room(cfg, Engine::Codex));
    let held: Vec<fs::File> = (1..=cfg.codex_slots)
        .map(|i| {
            let f = fs::File::create(cfg.jobs.join(".codex-slots").join(i.to_string())).unwrap();
            f.lock().unwrap();
            f
        })
        .collect();
    assert!(!crate::slot::room(cfg, Engine::Codex));
    drop(held);
    assert!(soon(|| crate::slot::room(cfg, Engine::Codex)));

    assert!(crate::slot::room(cfg, Engine::Claude));
    let slots = cfg.jobs.join(".slots");
    let held: Vec<fs::File> = (1..cfg.max_jobs)
        .map(|i| {
            let f = fs::File::create(slots.join(i.to_string())).unwrap();
            f.lock().unwrap();
            f
        })
        .collect();
    // One free, and it's the pipeline's.
    assert!(!crate::slot::room(cfg, Engine::Claude));
    drop(held);
    assert!(soon(|| crate::slot::room(cfg, Engine::Claude)));
}

/// Whether `f` holds within a second. A slot's lock dropped here can still
/// be held a moment by a process another test's thread is starting: it was
/// forked while the lock was held, and keeps it until it execs.
fn soon(mut f: impl FnMut() -> bool) -> bool {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(1);
    while !f() {
        if std::time::Instant::now() > until {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    true
}

// The drain picks from every repo's ready issues as each one's pass listed
// them (every page of its open issues), the pipeline's first, and asks
// GitHub for the one it picked before labelling it.
#[test]
fn the_drain_picks_across_every_repo() {
    let b = Box::new();
    let old = "2026-10-01T00:00:00Z";
    let mut game: Vec<Value> = (100..1200)
        .map(|n| found("sam/game", n, "player", &["ready"], old))
        .collect();
    game.push(found("sam/game", 4, "sam", &["ready"], old));
    let boxed = found("sam/box", 8, "sam", &["ready"], "2026-11-01T00:00:00Z");
    let p = b.pass();
    p.note_ready("sam/game", &game).unwrap();
    p.note_ready(
        "sam/box",
        &[boxed.clone(), found("sam/box", 9, "sam", &["later"], old)],
    )
    .unwrap();
    // Not open any more, as GitHub has it now: left, its repo's pass booked.
    let mut p = b.pass();
    p.drain().unwrap();
    assert!(!b.log().contains("labels[]="), "{}", b.log());
    assert!(p.wake.contains_key("sam/box"));
    on_github(&b, &boxed);
    b.pass().drain().unwrap();
    assert!(
        b.log()
            .contains("-X POST repos/sam/box/issues/8/labels -f labels[]=codex"),
        "{}",
        b.log()
    );
    // Its repo's list without it: the next picks from the rest.
    let b = Box::new();
    let p = b.pass();
    p.note_ready("sam/game", &game).unwrap();
    p.note_ready("sam/box", &[]).unwrap();
    on_github(&b, &game[game.len() - 1]);
    b.pass().drain().unwrap();
    assert!(
        b.log().contains("issues/4/labels -f labels[]=codex"),
        "{}",
        b.log()
    );
}

// What holds the drain back brings it back: a ready issue with no slot for
// it leaves its repo, said before the slots are looked at, for every turn's
// end to kick; GitHub not answering about the issue books a pass; one handed
// on but not yet started books a pass for when it's let go.
#[test]
fn the_drain_comes_back() {
    let state = |b: &Box| b.cfg.inbox.as_ref().unwrap().state.clone();
    let four = found("sam/game", 4, "sam", &["ready"], "2026-10-01T00:00:00Z");

    let b = Box::new();
    let mut p = b.pass();
    p.drain().unwrap();
    assert!(p.wake.is_empty(), "nothing ready, nothing booked");

    let b = Box::new();
    b.pass()
        .note_ready("sam/game", std::slice::from_ref(&four))
        .unwrap();
    fs::write(b.path("fail"), "repos/sam/game/issues/4\n").unwrap();
    let mut p = b.pass();
    p.drain().unwrap();
    assert!(
        p.wake.contains_key("sam/game"),
        "GitHub didn't say: {:?}",
        p.wake
    );
    fs::remove_file(b.path("fail")).unwrap();

    on_github(&b, &four);
    let cfg = &b.cfg;
    let held: Vec<fs::File> = (1..=cfg.codex_slots)
        .map(|i| {
            let dir = cfg.jobs.join(".codex-slots");
            fs::create_dir_all(&dir).unwrap();
            let f = fs::File::create(dir.join(i.to_string())).unwrap();
            f.lock().unwrap();
            f
        })
        .collect();
    b.pass().drain().unwrap();
    let waiting = state(&b).join(super::WAITING);
    assert_eq!(fs::read_to_string(&waiting).unwrap(), "sam/game\n");
    drop(held);
    assert!(
        soon(|| {
            b.pass().drain().unwrap();
            !waiting.exists()
        }),
        "handed on: no longer waiting"
    );
    assert!(b.log().contains("issues/4/labels -f labels[]=codex"));

    // Handed on, its job not started yet: at its limit (one here), the pass
    // books another for when the mark is let go.
    let mut p = b.pass();
    p.drain().unwrap();
    assert!(!p.wake.is_empty());
}

// One repo GitHub won't say about doesn't hold up the drain: its ready
// issues wait for its retry, and the next repo's is handed on.
#[test]
fn one_repo_cannot_hold_up_the_drain() {
    let b = Box::new();
    let old = "2026-10-01T00:00:00Z";
    let boxed = found("sam/box", 8, "sam", &["ready"], old);
    let game = found("sam/game", 4, "sam", &["ready"], old);
    let p = b.pass();
    p.note_ready(
        "sam/box",
        &[boxed.clone(), found("sam/box", 9, "sam", &["ready"], old)],
    )
    .unwrap();
    p.note_ready("sam/game", std::slice::from_ref(&game))
        .unwrap();
    on_github(&b, &game);
    fs::write(b.path("fail"), "repos/sam/box/issues\n").unwrap();
    let mut p = b.pass();
    p.drain().unwrap();
    assert!(p.wake.contains_key("sam/box"), "{:?}", p.wake);
    assert_eq!(
        b.log().matches("api repos/sam/box/issues/").count(),
        1,
        "{}",
        b.log()
    );
    assert!(
        b.log().contains("issues/4/labels -f labels[]=codex"),
        "{}",
        b.log()
    );
}

// A repo that won't take the label on the issue picked doesn't hold up the
// drain either: its record goes, its retry is booked, and the next repo's is
// handed on, one a pass still. Once GitHub takes the label again, the
// retry hands the first one on.
#[test]
fn a_label_refused_does_not_hold_up_the_drain() {
    let b = Box::new();
    let old = "2026-10-01T00:00:00Z";
    let boxed = found("sam/box", 8, "sam", &["ready"], old);
    let box_too = found("sam/box", 9, "sam", &["ready"], old);
    let game = found("sam/game", 4, "sam", &["ready"], old);
    let game_too = found("sam/game", 5, "sam", &["ready"], old);
    let p = b.pass();
    p.note_ready("sam/box", &[boxed.clone(), box_too.clone()])
        .unwrap();
    p.note_ready("sam/game", &[game.clone(), game_too.clone()])
        .unwrap();
    for i in [&boxed, &box_too, &game, &game_too] {
        on_github(&b, i);
    }
    let state = b.cfg.inbox.as_ref().unwrap().state.clone();
    fs::write(b.path("fail"), "repos/sam/box/issues/8/labels\n").unwrap();
    let mut p = b.pass();
    p.drain().unwrap();
    let log = b.log();
    assert!(log.contains("issues/8/labels -f labels[]=codex"), "{log}");
    assert!(!state.join("sam_box-8").exists(), "its record goes");
    assert!(p.wake.contains_key("sam/box"), "{:?}", p.wake);
    assert!(!log.contains("api repos/sam/box/issues/9"), "{log}");
    assert!(log.contains("issues/4/labels -f labels[]=codex"), "{log}");
    assert!(state.join("sam_game-4/drained").exists());
    assert_eq!(log.matches("labels[]=codex").count(), 2, "{log}");

    // Taken again: the retry hands the refused one on, the pipeline's first.
    fs::remove_file(b.path("fail")).unwrap();
    fs::remove_dir_all(state.join("sam_game-4")).unwrap();
    let before = b.log().len();
    b.pass().drain().unwrap();
    let log = b.log()[before..].to_owned();
    assert!(log.contains("issues/8/labels -f labels[]=codex"), "{log}");
    assert_eq!(log.matches("labels[]=codex").count(), 1, "{log}");
    assert!(state.join("sam_box-8/drained").exists());
}
