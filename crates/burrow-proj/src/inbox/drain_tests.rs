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

// One handed on whose job never starts holds its place while GitHub still
// has it open with the engine's label: the drain doesn't hand on another
// that would stall the same way. Once the label's off, it's let go and the
// next is handed on.
#[test]
fn a_stalled_hand_on_holds_its_place() {
    let b = Box::new();
    let state = b.cfg.inbox.as_ref().unwrap().state.clone();
    let old = "2026-10-01T00:00:00Z";
    let four = found("sam/game", 4, "sam", &["ready"], old);
    let five = found("sam/game", 5, "sam", &["ready"], old);
    b.pass()
        .note_ready("sam/game", &[four.clone(), five.clone()])
        .unwrap();
    on_github(&b, &four);
    on_github(&b, &five);
    let backdate = |n: u64| {
        let mark = state.join(format!("sam_game-{n}/drained"));
        let past = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        fs::File::options()
            .write(true)
            .open(&mark)
            .unwrap()
            .set_modified(past)
            .unwrap();
    };
    b.pass().drain().unwrap();
    assert!(b.log().contains("issues/4/labels -f labels[]=codex"));
    assert_eq!(
        fs::read_to_string(state.join("sam_game-4/drained")).unwrap(),
        "sam/game#4\n"
    );
    // Its job never started, and GitHub has the label on still.
    backdate(4);
    on_github(&b, &found("sam/game", 4, "sam", &["ready", "codex"], old));
    let before = b.log().len();
    b.pass().drain().unwrap();
    let log = b.log()[before..].to_owned();
    assert!(!log.contains("labels[]=codex"), "{log}");
    assert!(state.join("sam_game-4/drained").exists());
    // The label's off: its old mark is let go, and the oldest ready one
    // (itself again, as nothing else holds it) handed on afresh.
    on_github(&b, &four);
    let before = b.log().len();
    b.pass().drain().unwrap();
    let log = b.log()[before..].to_owned();
    assert!(log.contains("issues/4/labels -f labels[]=codex"), "{log}");
    let mark = state.join("sam_game-4/drained");
    let made = crate::mtime(&mark).unwrap();
    assert!(crate::now() - made < 60, "a fresh mark: {log}");
}

// A lane's handed brief owns its references until its job ends.
#[test]
fn running_handed_references_are_not_drained() {
    let b = Box::new();
    b.job("lane", "done", "finished");
    let d = b.record("lane");
    super::Pass::hand(&d, "Deliver https://github.com/sam/box/issues/7 and #8/#10; sam/game#9 is elsewhere. https://example.org/docs/#11").unwrap();
    fs::write(b.path("active"), "").unwrap();
    fs::write(b.cfg.jobs.join("lane/1.slot"), "").unwrap();
    let seven = found("sam/box", 7, "sam", &["ready"], "2026-10-01T00:00:00Z");
    // The lane's own record must not be the reason #7 is skipped.
    let lane = d.with_file_name("sam_box-100");
    fs::rename(&d, &lane).unwrap();
    fs::write(lane.join("issue"), "sam/box#100\n").unwrap();
    let t = b.cfg.inbox.as_ref().unwrap().triage.as_ref().unwrap();
    let p = b.pass();
    assert_eq!(p.pick_ready(t, std::slice::from_ref(&seven)), None);
    let mut unsorted = seven.clone();
    unsorted["labels"] = json!([]);
    assert!(p.sorted(t, &unsorted, &p.claimed("sam/box")));
    for (repo, n, claimed) in [
        ("sam/box", 8, true),
        ("sam/box", 10, true),
        ("sam/box", 9, false),
        ("sam/box", 11, false),
        ("sam/game", 7, false),
    ] {
        let i = found(repo, n, "sam", &["ready"], "2026-10-01T00:00:00Z");
        assert_eq!(p.pick_ready(t, &[i]).is_none(), claimed);
    }
    p.note_ready("sam/box", std::slice::from_ref(&seven))
        .unwrap();
    on_github(&b, &seven);
    b.pass().drain().unwrap();
    assert!(!b.log().contains("labels[]=codex"));
    // Waiting for a slot still owns the brief.
    fs::remove_file(b.cfg.jobs.join("lane/1.slot")).unwrap();
    assert_eq!(b.pass().pick_ready(t, std::slice::from_ref(&seven)), None);
    fs::remove_file(b.path("active")).unwrap();
    assert!(!b.pass().sorted(t, &unsorted, &b.pass().claimed("sam/box")));
    b.pass().drain().unwrap();
    assert!(b.log().contains("issues/7/labels -f labels[]=codex"));
}

fn hold_engine(b: &Box, engine: Engine) -> Vec<fs::File> {
    let (dir, count) = match engine {
        Engine::Claude => (".slots", b.cfg.max_jobs),
        Engine::Codex => (".codex-slots", b.cfg.codex_slots),
    };
    let dir = b.cfg.jobs.join(dir);
    fs::create_dir_all(&dir).unwrap();
    (1..=count)
        .map(|n| {
            let f = fs::File::create(dir.join(n.to_string())).unwrap();
            f.lock().unwrap();
            f
        })
        .collect()
}

#[test]
fn drain_admission_uses_the_candidate_role_instead_of_the_enrollment_engine() {
    for (opener, labels, roles, mapping, selected) in [
        ("worker[bot]", vec!["ready"], "", None, Engine::Claude),
        (
            "worker[bot]",
            vec!["ready"],
            "machine-inbox codex",
            None,
            Engine::Codex,
        ),
        (
            "worker[bot]",
            vec!["ready"],
            "machine-inbox claude\nmachine-inbox@SAM/GAME codex",
            None,
            Engine::Codex,
        ),
        (
            "sam",
            vec!["ready", "kind"],
            "special claude\nspecial@sam/game codex",
            Some("kind"),
            Engine::Codex,
        ),
        // The label the drain is about to add can itself select a role.
        (
            "sam",
            vec!["ready"],
            "special claude",
            Some("codex"),
            Engine::Claude,
        ),
        // Ordinary human issues keep the enrollment label's engine.
        (
            "sam",
            vec!["ready"],
            "machine-inbox claude",
            None,
            Engine::Codex,
        ),
    ] {
        for full in [Engine::Claude, Engine::Codex] {
            let mut b = Box::new();
            b.cfg.roles = b.path("roles");
            fs::write(&b.cfg.roles, roles).unwrap();
            let inbox = b.cfg.inbox.as_mut().unwrap();
            inbox.authors.push("worker[bot]".into());
            if let Some(label) = mapping {
                inbox.roles.insert(label.into(), "special".into());
            }
            let i = found("sam/game", 4, opener, &labels, "2026-10-01T00:00:00Z");
            b.pass()
                .note_ready("sam/game", std::slice::from_ref(&i))
                .unwrap();
            on_github(&b, &i);
            let _held = hold_engine(&b, full);
            b.pass().drain().unwrap();
            let admitted = b.log().contains("issues/4/labels -f labels[]=codex");
            assert_eq!(
                admitted,
                selected != full,
                "{roles:?}, {mapping:?}, {opener}, full={full:?}: {}",
                b.log()
            );
            assert_eq!(
                b.cfg
                    .inbox
                    .as_ref()
                    .unwrap()
                    .state
                    .join(super::WAITING)
                    .exists(),
                !admitted
            );
        }
    }
}

#[test]
fn drain_usage_wakes_and_overflow_follow_the_selected_engine() {
    for (roles, selected) in [
        ("machine-inbox claude", Engine::Claude),
        (
            "machine-inbox claude\nmachine-inbox@sam/game codex",
            Engine::Codex,
        ),
        ("machine-inbox claude over 80 codex", Engine::Codex),
    ] {
        for full_codex in [false, true] {
            let mut b = Box::new();
            b.cfg.roles = b.path("roles");
            fs::write(&b.cfg.roles, roles).unwrap();
            b.cfg
                .inbox
                .as_mut()
                .unwrap()
                .authors
                .push("worker[bot]".into());
            fs::create_dir_all(&b.cfg.jobs).unwrap();
            let until = crate::now() + 3600;
            fs::write(
                b.cfg.jobs.join(crate::usage::FILE),
                json!({"at":crate::now(), "until":until, "said":"spent", "job":"old"}).to_string(),
            )
            .unwrap();
            let meters = b.path("meters");
            fs::create_dir_all(&meters).unwrap();
            fs::write(
                meters.join("codex.json"),
                json!({"windows":{}, "credits":true, "read_at":crate::now()}).to_string(),
            )
            .unwrap();
            b.cfg.meters = Some(meters);
            let i = found(
                "sam/game",
                4,
                "worker[bot]",
                &["ready"],
                "2026-10-01T00:00:00Z",
            );
            b.pass()
                .note_ready("sam/game", std::slice::from_ref(&i))
                .unwrap();
            on_github(&b, &i);
            let _held = full_codex.then(|| hold_engine(&b, Engine::Codex));
            let mut p = b.pass();
            p.drain().unwrap();
            let admitted = b.log().contains("issues/4/labels -f labels[]=codex");
            assert_eq!(
                admitted,
                selected == Engine::Codex && !full_codex,
                "{roles}: {}",
                b.log()
            );
            if selected == Engine::Claude {
                assert_eq!(p.wake.get("sam/game"), Some(&until));
            } else {
                assert_ne!(p.wake.get("sam/game"), Some(&until));
            }
            assert_eq!(
                b.cfg
                    .inbox
                    .as_ref()
                    .unwrap()
                    .state
                    .join(super::WAITING)
                    .exists(),
                !admitted
            );
        }
    }
}

#[test]
fn a_blocked_role_does_not_block_another_engine_or_bypass_the_drain_limit() {
    let mut b = Box::new();
    b.cfg.roles = b.path("roles");
    fs::write(&b.cfg.roles, "").unwrap();
    b.cfg
        .inbox
        .as_mut()
        .unwrap()
        .authors
        .push("worker[bot]".into());
    let older = found(
        "sam/game",
        4,
        "worker[bot]",
        &["ready"],
        "2026-10-01T00:00:00Z",
    );
    let newer = found("sam/game", 5, "sam", &["ready"], "2026-11-01T00:00:00Z");
    b.pass()
        .note_ready("sam/game", &[older.clone(), newer.clone()])
        .unwrap();
    on_github(&b, &older);
    on_github(&b, &newer);
    let held = hold_engine(&b, Engine::Claude);
    b.pass().drain().unwrap();
    assert!(
        !b.log().contains("issues/4/labels -f labels[]=codex"),
        "{}",
        b.log()
    );
    assert!(
        b.log().contains("issues/5/labels -f labels[]=codex"),
        "{}",
        b.log()
    );
    let state = &b.cfg.inbox.as_ref().unwrap().state;
    assert!(
        state.join(super::WAITING).exists(),
        "the blocked engine still needs a wake-up"
    );
    drop(held);
    b.pass().drain().unwrap();
    assert!(
        !b.log().contains("issues/4/labels -f labels[]=codex"),
        "drainJobs remains one: {}",
        b.log()
    );
}

// A finished turn parked on GitHub still owns its brief until its live wait goes.
#[test]
fn awaiting_handed_references_are_not_drained() {
    let b = Box::new();
    b.job("lane", "done", "waiting for review");
    let d = b.record("lane");
    let lane = d.with_file_name("sam_box-100");
    fs::rename(d, &lane).unwrap();
    fs::write(lane.join("issue"), "sam/box#100\n").unwrap();
    super::Pass::hand(&lane, "Deliver #7").unwrap();
    let job = b.cfg.jobs.join("lane");
    fs::write(job.join("1.end"), "await\n").unwrap();
    let wait = format!("sam/box#200\t{}\t{{\"state\":\"open\"}}\n", crate::now());
    let seven = found("sam/box", 7, "sam", &["ready"], "2026-10-01T00:00:00Z");
    let t = b.cfg.inbox.as_ref().unwrap().triage.as_ref().unwrap();
    let p = b.pass();
    // A declaration for a future turn and an empty live wait do not own work.
    fs::write(job.join("await.next"), &wait).unwrap();
    fs::write(job.join("await"), "").unwrap();
    assert!(p.pick_ready(t, std::slice::from_ref(&seven)).is_some());
    fs::write(job.join("await"), &wait).unwrap();
    assert_eq!(p.pick_ready(t, std::slice::from_ref(&seven)), None);
    let mut unsorted = seven.clone();
    unsorted["labels"] = json!([]);
    assert!(p.sorted(t, &unsorted, &p.claimed("sam/box")));
    p.note_ready("sam/box", std::slice::from_ref(&seven))
        .unwrap();
    on_github(&b, &seven);
    b.pass().drain().unwrap();
    assert!(!b.log().contains("labels[]=codex"));
    // Stopping or finishing the job removes its live wait, releasing the brief.
    fs::remove_file(job.join("await")).unwrap();
    assert!(!p.sorted(t, &unsorted, &p.claimed("sam/box")));
    b.pass().drain().unwrap();
    assert!(b.log().contains("issues/7/labels -f labels[]=codex"));
}

#[test]
fn paused_ready_issues_wait_without_relabelling() {
    let mut b = Box::new();
    let paused = found("sam/box", 8, "sam", &["ready"], "2026-10-01T00:00:00Z");
    let active = found("sam/game", 4, "sam", &["ready"], "2026-11-01T00:00:00Z");
    b.pass().note_ready("sam/box", &[paused.clone()]).unwrap();
    b.pass().note_ready("sam/game", &[active.clone()]).unwrap();
    b.cfg.paused = vec!["SAM/BOX".into()];
    on_github(&b, &active);
    b.pass().drain().unwrap();
    assert!(
        b.log().contains("issues/4/labels -f labels[]=codex"),
        "{}",
        b.log()
    );
    assert!(!b.log().contains("repos/sam/box/issues/8"), "{}", b.log());
    // The original ready list survives the pause and is eligible again.
    b.cfg.paused.clear();
    let t = b.cfg.inbox.as_ref().unwrap().triage.clone().unwrap();
    assert_eq!(
        b.pass().pick_ready(&t, &[paused]),
        Some(("sam/box".into(), "8".into()))
    );
}

#[test]
fn a_paused_inbox_pass_and_triage_do_no_work() {
    let mut b = Box::new();
    b.cfg.paused = vec!["SAM/BOX".into()];
    b.pass().triage("sam/box", &[issue(7, "sam", &[])]).unwrap();
    b.pass().run("sam/box").unwrap();
    assert!(b.log().is_empty(), "{}", b.log());
}
