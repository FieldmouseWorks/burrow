//! The drain: which ready issue it hands on, when there's room for it, and
//! that nothing one repo does holds up the rest, against the stand-ins in
//! [`super::sorting_tests`].

use crate::inbox::record::files;
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
    let mut p = b.pass();
    let old = "2026-10-01T00:00:00Z";
    let new = "2026-11-01T00:00:00Z";
    let mut pick = |items: &[Value]| p.pick_ready(items);

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
        found("sam/game", 4, "sam", &["ready", "claude"], old),
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

// Room for the drain: a slot free beyond the pipeline's and nobody in line.
#[test]
fn room_for_the_drain() {
    let b = Box::new();
    let cfg = &b.cfg;
    assert!(crate::slot::room(cfg, Engine::Claude, false));
    let slots = cfg.jobs.join(".slots");
    let held: Vec<fs::File> = (1..cfg.max_jobs)
        .map(|i| {
            let f = fs::File::create(slots.join(i.to_string())).unwrap();
            f.lock().unwrap();
            f
        })
        .collect();
    // One free, and it's the pipeline's.
    assert!(!crate::slot::room(cfg, Engine::Claude, false));
    drop(held);
    assert!(soon(|| crate::slot::room(cfg, Engine::Claude, false)));
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
    // Closed, as GitHub has it now: left (and its list put right).
    let mut p = b.pass();
    p.drain().unwrap();
    assert!(!b.log().contains("labels[]="), "{}", b.log());
    on_github(&b, &boxed);
    p.note_ready("sam/box", std::slice::from_ref(&boxed))
        .unwrap();
    b.pass().drain().unwrap();
    assert!(
        b.log()
            .contains("-X POST repos/sam/box/issues/8/labels -f labels[]=claude"),
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
        b.log().contains("issues/4/labels -f labels[]=claude"),
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
    let held = hold_engine(&b, Engine::Claude);
    b.pass().drain().unwrap();
    let waiting = super::record::files::drain_waiting(&state(&b));
    assert_eq!(fs::read_to_string(&waiting).unwrap(), "sam/game\n");
    drop(held);
    assert!(
        soon(|| {
            b.pass().drain().unwrap();
            !waiting.exists()
        }),
        "handed on: no longer waiting"
    );
    assert!(b.log().contains("issues/4/labels -f labels[]=claude"));

    // Handed on, its job not started yet: at its limit (one here), the pass
    // books another for when the mark is let go.
    on_github(
        &b,
        &found(
            "sam/game",
            4,
            "sam",
            &["ready", "claude"],
            "2026-10-01T00:00:00Z",
        ),
    );
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
        b.log().contains("issues/4/labels -f labels[]=claude"),
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
    assert!(log.contains("issues/8/labels -f labels[]=claude"), "{log}");
    assert!(!state.join("sam_box-8").exists(), "its record goes");
    assert!(p.wake.contains_key("sam/box"), "{:?}", p.wake);
    assert!(!log.contains("api repos/sam/box/issues/9"), "{log}");
    assert!(log.contains("issues/4/labels -f labels[]=claude"), "{log}");
    assert!(state.join("sam_game-4/drained").exists());
    assert_eq!(log.matches("labels[]=claude").count(), 2, "{log}");

    // Taken again: the retry hands the refused one on, the pipeline's first.
    fs::remove_file(b.path("fail")).unwrap();
    fs::remove_dir_all(state.join("sam_game-4")).unwrap();
    let before = b.log().len();
    b.pass().drain().unwrap();
    let log = b.log()[before..].to_owned();
    assert!(log.contains("issues/8/labels -f labels[]=claude"), "{log}");
    assert_eq!(log.matches("labels[]=claude").count(), 1, "{log}");
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
    assert!(b.log().contains("issues/4/labels -f labels[]=claude"));
    assert_eq!(
        fs::read_to_string(state.join("sam_game-4/drained")).unwrap(),
        "sam/game#4\n"
    );
    // Its job never started, and GitHub has the label on still.
    backdate(4);
    on_github(&b, &found("sam/game", 4, "sam", &["ready", "claude"], old));
    let before = b.log().len();
    b.pass().drain().unwrap();
    let log = b.log()[before..].to_owned();
    assert!(!log.contains("labels[]=claude"), "{log}");
    assert!(state.join("sam_game-4/drained").exists());
    // Removing the last engine label parks #4 and releases its reservation.
    // The next ready issue can start, while #4 stays excluded.
    on_github(&b, &four);
    let before = b.log().len();
    b.pass().drain().unwrap();
    let log = b.log()[before..].to_owned();
    assert!(!log.contains("issues/4/labels -f labels[]=claude"), "{log}");
    assert!(log.contains("issues/5/labels -f labels[]=claude"), "{log}");
    assert!(!state.join("sam_game-4/drained").exists());
    let mark = state.join("sam_game-5/drained");
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
    fs::write(lane.join(files::File::Issue), "sam/box#100\n").unwrap();
    let t = b.cfg.inbox.as_ref().unwrap().triage.as_ref().unwrap();
    let mut p = b.pass();
    assert_eq!(p.pick_ready(std::slice::from_ref(&seven)), None);
    let mut unsorted = seven.clone();
    unsorted["labels"] = json!([]);
    assert!(p.sorted("sam/box", t, &unsorted, &p.claimed("sam/box")));
    for (repo, n, claimed) in [
        ("sam/box", 8, true),
        ("sam/box", 10, true),
        ("sam/box", 9, false),
        ("sam/box", 11, false),
        ("sam/game", 7, false),
    ] {
        let i = found(repo, n, "sam", &["ready"], "2026-10-01T00:00:00Z");
        assert_eq!(p.pick_ready(&[i]).is_none(), claimed);
    }
    p.note_ready("sam/box", std::slice::from_ref(&seven))
        .unwrap();
    on_github(&b, &seven);
    b.pass().drain().unwrap();
    assert!(!b.log().contains("labels[]=claude"));
    // Waiting for a slot still owns the brief.
    fs::remove_file(b.cfg.jobs.join("lane/1.slot")).unwrap();
    assert_eq!(b.pass().pick_ready(std::slice::from_ref(&seven)), None);
    fs::remove_file(b.path("active")).unwrap();
    assert!(
        !b.pass()
            .sorted("sam/box", t, &unsorted, &b.pass().claimed("sam/box"))
    );
    b.pass().drain().unwrap();
    assert!(b.log().contains("issues/7/labels -f labels[]=claude"));
}

fn hold_engine(b: &Box, engine: Engine) -> Vec<fs::File> {
    let (dir, count) = match engine {
        Engine::Claude | Engine::ClaudeApi => (".slots", b.cfg.max_jobs),
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
fn drain_admission_follows_room_on_the_candidates_engine() {
    for (opener, labels, roles, mapping) in [
        ("worker[bot]", vec!["ready"], "", None),
        ("worker[bot]", vec!["ready"], "machine-inbox claude", None),
        (
            "sam",
            vec!["ready", "kind"],
            "special claude\nspecial@sam/game claude",
            Some("kind"),
        ),
        // The label the drain is about to add can itself select a role.
        ("sam", vec!["ready"], "special claude", Some("claude")),
    ] {
        for full in [false, true] {
            let mut b = Box::new();
            let dir = b.path("settings");
            crate::config::tests::routes(&mut b.cfg, &dir, roles);
            let inbox = b.cfg.inbox.as_mut().unwrap();
            inbox
                .authors
                .push(burrow_github::fake::author("worker[bot]"));
            if let Some(label) = mapping {
                inbox.roles.insert(label.into(), "special".into());
            }
            let i = found("sam/game", 4, opener, &labels, "2026-10-01T00:00:00Z");
            b.pass()
                .note_ready("sam/game", std::slice::from_ref(&i))
                .unwrap();
            on_github(&b, &i);
            let _held = full.then(|| hold_engine(&b, Engine::Claude));
            b.pass().drain().unwrap();
            let admitted = b.log().contains("issues/4/labels -f labels[]=claude");
            assert_eq!(
                admitted,
                !full,
                "{roles:?}, {mapping:?}, {opener}, full={full}: {}",
                b.log()
            );
            assert_eq!(
                super::record::files::drain_waiting(&b.cfg.inbox.as_ref().unwrap().state).exists(),
                !admitted
            );
        }
    }
}

#[test]
fn a_usage_hold_books_the_drain_for_its_reset() {
    let mut b = Box::new();
    b.cfg
        .inbox
        .as_mut()
        .unwrap()
        .authors
        .push(burrow_github::fake::author("worker[bot]"));
    fs::create_dir_all(&b.cfg.jobs).unwrap();
    let until = crate::now() + 3600;
    fs::write(
        b.cfg.jobs.join(crate::usage::FILE),
        json!({"at":crate::now(), "until":until, "said":"spent", "job":"old"}).to_string(),
    )
    .unwrap();
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
    let mut p = b.pass();
    p.drain().unwrap();
    assert!(!b.log().contains("labels[]="), "{}", b.log());
    assert_eq!(p.wake.get("sam/game"), Some(&until));
    assert!(super::record::files::drain_waiting(&b.cfg.inbox.as_ref().unwrap().state).exists());
}

// A ready issue routed to `claude-api` whose credit has no room books the
// drain for when the clock gives it back: nothing else would come on an idle
// machine. A run going, its spend unknown, books nothing: its turn's end
// kicks the drain. The owner's pause holds it too, and books its end.
#[test]
fn a_spent_api_credit_books_the_drain_for_when_it_opens() {
    const M: i64 = 1_000_000;
    let midnight = |days: i64| {
        let today = jiff::Timestamp::now()
            .to_zoned_datetime(jiff::tz::TimeZone::UTC)
            .date();
        today.checked_add(jiff::Span::new().days(days)).unwrap()
    };
    let at = |day: jiff::civil::Date| {
        day.to_zoned_datetime(jiff::tz::TimeZone::UTC)
            .unwrap()
            .timestamp()
            .as_second()
    };
    // $20 expiring in two days with $5 kept: $10 a day.
    let renews = midnight(2);
    let paused = crate::now() + 3600;
    // Each run as (its budget, its cost once in), written once the credit
    // is set: a run from before the setting is no run of its cycle.
    let cases = [
        (
            "today's allowance spent",
            Some((10, Some(10))),
            false,
            Some(at(midnight(1))),
        ),
        (
            "spent down to the reserve",
            Some((15, Some(15))),
            false,
            Some(at(renews)),
        ),
        // Going with no budget recorded: its spend can't be known.
        (
            "a run going, its spend unknown",
            Some((-1, None)),
            false,
            None,
        ),
        ("room, slots full", None, false, None),
        ("the owner's pause", None, true, Some(paused)),
    ];
    for (name, run, pause, wake) in cases {
        let mut b = Box::new();
        let dir = b.path("settings");
        crate::config::tests::routes(&mut b.cfg, &dir, "machine-inbox claude-api");
        crate::config::tests::set(&mut b.cfg, &dir, &[&format!("credit 20 {renews} 200 5")]);
        b.cfg
            .inbox
            .as_mut()
            .unwrap()
            .authors
            .push(burrow_github::fake::author("worker[bot]"));
        fs::create_dir_all(&b.cfg.jobs).unwrap();
        if let Some((budget, cost)) = run {
            let mut marker = json!({"type":"burrow.run","at":crate::now()});
            if budget >= 0 {
                marker["budget"] = json!(budget * M);
            }
            let mut stream = vec![marker];
            stream.extend(
                cost.map(|usd| json!({"type":"result","session_id":"a","total_cost_usd":usd})),
            );
            let job = b.cfg.jobs.join("spent");
            fs::create_dir_all(&job).unwrap();
            fs::write(job.join("1.engine"), "claude-api").unwrap();
            fs::write(job.join("1.started"), crate::now().to_string()).unwrap();
            let text: String = stream.iter().map(|r| format!("{r}\n")).collect();
            fs::write(job.join("1.jsonl"), text).unwrap();
        }
        if pause {
            crate::usage::hold(&b.cfg, paused, "save usage").unwrap();
        }
        let _held = run.is_none().then(|| hold_engine(&b, Engine::ClaudeApi));
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
        let mut p = b.pass();
        p.drain().unwrap();
        assert!(!b.log().contains("labels[]="), "{name}: {}", b.log());
        assert_eq!(p.wake.get("sam/game").copied(), wake, "{name}");
        assert!(
            super::record::files::drain_waiting(&b.cfg.inbox.as_ref().unwrap().state).exists(),
            "{name}"
        );
    }
}

// A finished turn parked on GitHub still owns its brief until its live wait goes.
#[test]
fn awaiting_handed_references_are_not_drained() {
    let b = Box::new();
    b.job("lane", "done", "waiting for review");
    let d = b.record("lane");
    let lane = d.with_file_name("sam_box-100");
    fs::rename(d, &lane).unwrap();
    fs::write(lane.join(files::File::Issue), "sam/box#100\n").unwrap();
    super::Pass::hand(&lane, "Deliver #7").unwrap();
    let job = b.cfg.jobs.join("lane");
    fs::write(job.join("1.end"), "await\n").unwrap();
    let wait = format!("sam/box#200\t{}\t{{\"state\":\"open\"}}\n", crate::now());
    let seven = found("sam/box", 7, "sam", &["ready"], "2026-10-01T00:00:00Z");
    let t = b.cfg.inbox.as_ref().unwrap().triage.as_ref().unwrap();
    let mut p = b.pass();
    // A declaration for a future turn and an empty live wait do not own work.
    fs::write(job.join("await.next"), &wait).unwrap();
    fs::write(job.join("await"), "").unwrap();
    assert!(p.pick_ready(std::slice::from_ref(&seven)).is_some());
    fs::write(job.join("await"), &wait).unwrap();
    assert_eq!(p.pick_ready(std::slice::from_ref(&seven)), None);
    let mut unsorted = seven.clone();
    unsorted["labels"] = json!([]);
    assert!(p.sorted("sam/box", t, &unsorted, &p.claimed("sam/box")));
    p.note_ready("sam/box", std::slice::from_ref(&seven))
        .unwrap();
    on_github(&b, &seven);
    b.pass().drain().unwrap();
    assert!(!b.log().contains("labels[]=claude"));
    // Stopping or finishing the job removes its live wait, releasing the brief.
    fs::remove_file(job.join("await")).unwrap();
    assert!(!p.sorted("sam/box", t, &unsorted, &p.claimed("sam/box")));
    b.pass().drain().unwrap();
    assert!(b.log().contains("issues/7/labels -f labels[]=claude"));
}

#[test]
fn paused_ready_issues_wait_without_relabelling() {
    let mut b = Box::new();
    let paused = found("sam/box", 8, "sam", &["ready"], "2026-10-01T00:00:00Z");
    let active = found("sam/game", 4, "sam", &["ready"], "2026-11-01T00:00:00Z");
    b.pass()
        .note_ready("sam/box", std::slice::from_ref(&paused))
        .unwrap();
    b.pass()
        .note_ready("sam/game", std::slice::from_ref(&active))
        .unwrap();
    b.cfg.settings = Some(burrow_settings::Config {
        overlay: b.path("overlay.json"),
        allowed: vec!["sam/game".into(), "sam/burrow".into()],
        pipeline_repos: vec!["sam/box".into(), "sam/burrow".into()],
        ..Default::default()
    });
    on_github(&b, &active);
    b.pass().drain().unwrap();
    assert!(
        b.log().contains("issues/4/labels -f labels[]=claude"),
        "{}",
        b.log()
    );
    assert!(!b.log().contains("repos/sam/box/issues/8"), "{}", b.log());
    // The original ready list survives the pause and is eligible again.
    b.cfg
        .settings
        .as_mut()
        .unwrap()
        .allowed
        .push("sam/box".into());
    assert_eq!(
        b.pass().pick_ready(&[paused]),
        Some(("sam/box".into(), "8".into()))
    );
}

#[test]
fn paused_records_keep_results_without_holding_shared_capacity() {
    let mut b = Box::new();
    let sorting = super::sorting_tests::sorting(&b, "deferred verdict");
    let state = b.cfg.inbox.as_ref().unwrap().state.clone();
    let drained = state.join("sam_box-8");
    fs::create_dir_all(&drained).unwrap();
    fs::write(drained.join(files::File::Drained), "sam/box#8\n").unwrap();
    fs::write(drained.join(files::File::Job), "s1\n").unwrap();
    b.cfg.settings = Some(burrow_settings::Config {
        overlay: b.path("overlay.json"),
        allowed: vec!["sam/game".into(), "sam/burrow".into()],
        pipeline_repos: vec!["sam/box".into(), "sam/burrow".into()],
        ..Default::default()
    });
    b.cfg
        .inbox
        .as_mut()
        .unwrap()
        .triage
        .as_mut()
        .unwrap()
        .at_once = 1;
    let p = b.pass();
    assert!(!p.holds_sorting_slot(&sorting));
    assert!(!p.record_uses_capacity(&drained));
    // A genuinely active paused turn still consumes shared capacity.
    fs::write(b.path("active"), "").unwrap();
    fs::write(b.cfg.jobs.join("s1/1.slot"), "1").unwrap();
    assert!(p.holds_sorting_slot(&sorting));
    assert!(p.record_uses_capacity(&drained));
    // A queued automated worker of a paused repository is held, not counted.
    fs::write(b.cfg.jobs.join("s1/role"), "work\n").unwrap();
    fs::remove_file(b.cfg.jobs.join("s1/1.slot")).unwrap();
    assert!(!p.record_uses_capacity(&drained));
    fs::remove_file(b.cfg.jobs.join("s1/role")).unwrap();
    fs::remove_file(b.path("active")).unwrap();
    let ready = found("sam/game", 4, "sam", &["ready"], "2026-11-01T00:00:00Z");
    p.note_ready("sam/game", std::slice::from_ref(&ready))
        .unwrap();
    on_github(&b, &ready);
    b.pass().drain().unwrap();
    assert!(
        b.log().contains("issues/4/labels -f labels[]=claude"),
        "{}",
        b.log()
    );
    assert!(drained.join(files::File::Job).exists());
    assert!(sorting.join(files::File::Job).exists());
    // A paused handoff that never got a job reserves no capacity either.
    fs::remove_file(drained.join(files::File::Job)).unwrap();
    assert!(!b.pass().record_uses_capacity(&drained));
    // Triage uses this predicate to budget atOnce before starting work.
    assert_eq!(
        super::dirs(&state.join(".triage"), "")
            .iter()
            .filter(|d| b.pass().holds_sorting_slot(d))
            .count(),
        0
    );
    assert!(sorting.join(files::File::Job).exists());
    assert!(!b.posted().contains("deferred verdict"));
}

#[test]
fn priorities_precede_pipeline_and_age_and_drive_queue() {
    let b = Box::new();
    let mut p = b.pass();
    let t = b.cfg.inbox.as_ref().unwrap().triage.clone().unwrap();
    let items = [
        found("sam/box", 1, "sam", &["ready"], "2026-01-01"),
        found(
            "sam/game",
            2,
            "sam",
            &["ready", "priority: high"],
            "2026-03-01",
        ),
        found(
            "sam/box",
            3,
            "sam",
            &["ready", "priority: high"],
            "2026-05-01",
        ),
        found(
            "sam/box",
            4,
            "sam",
            &["ready", "priority: high"],
            "2026-04-01",
        ),
        found(
            "sam/box",
            5,
            "sam",
            &["ready", "priority: low"],
            "2025-01-01",
        ),
    ];
    for i in &items {
        let (repo, n) = p.pick_ready(std::slice::from_ref(i)).unwrap();
        p.note_ready(
            &repo,
            &items
                .iter()
                .filter(|i| i["repository_url"].as_str().unwrap().ends_with(&repo))
                .cloned()
                .collect::<Vec<_>>(),
        )
        .unwrap();
        on_github(&b, i);
        assert!(!n.is_empty());
    }
    // The snapshot's queue is the drain's order, with what put each there.
    use crate::snapshot::Priority::{High, Low, Medium};
    let queue: Vec<_> = p
        .view(&t, crate::now())
        .queue
        .into_iter()
        .map(|r| (format!("{}#{}", r.repo, r.number), r.priority, r.pipeline))
        .collect();
    assert_eq!(
        queue,
        [
            ("sam/box#4".to_owned(), High, true),
            ("sam/box#3".to_owned(), High, true),
            ("sam/game#2".to_owned(), High, false),
            ("sam/box#1".to_owned(), Medium, true),
            ("sam/box#5".to_owned(), Low, true),
        ]
    );
    b.pass().drain().unwrap();
    assert!(
        b.log().contains("issues/4/labels -f labels[]=claude"),
        "{}",
        b.log()
    );
}

#[test]
fn drain_counts_turns_instead_of_parked_jobs() {
    for state in ["await", "ask", "queued", "running", "paused"] {
        let mut b = Box::new();
        b.cfg
            .inbox
            .as_mut()
            .unwrap()
            .triage
            .as_mut()
            .unwrap()
            .drain_jobs = 3;
        for n in 1..=3 {
            let id = format!("drained-{n}");
            let dir = b
                .cfg
                .inbox
                .as_ref()
                .unwrap()
                .state
                .join(format!("sam_box-{n}"));
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join(files::File::Drained), format!("sam/box#{n}\n")).unwrap();
            fs::write(dir.join(files::File::Job), &id).unwrap();
            let job = crate::job::Job::new(&b.cfg, &id);
            fs::create_dir_all(&job.dir).unwrap();
            crate::policy::bind(
                &job,
                crate::policy::Identity::Repository("sam/box".into()),
                crate::policy::Kind::Automated,
            )
            .unwrap();
            fs::write(job.path("turns"), "1").unwrap();
            fs::write(job.turn("1", "outcome"), state).unwrap();
            if state == "running" || state == "paused" {
                fs::write(job.turn("1", "slot"), "1").unwrap();
            }
            if state == "paused" {
                crate::policy::mark(&job, "running", false, None).unwrap();
                b.cfg
                    .settings
                    .as_mut()
                    .unwrap()
                    .allowed
                    .retain(|r| r != "sam/box");
                assert!(super::job_paused(&b.cfg, &id));
            } else {
                assert!(!super::job_paused(&b.cfg, &id));
            }
        }
        // The paused case models a parked job after its unit has stopped.
        if matches!(state, "queued" | "running") {
            fs::write(b.path("active"), "").unwrap();
        }
        let ready = found("sam/game", 4, "sam", &["ready"], "2026-11-01");
        b.pass()
            .note_ready("sam/game", std::slice::from_ref(&ready))
            .unwrap();
        on_github(&b, &ready);
        let blocked = matches!(state, "queued" | "running");
        let view = b.pass().view(
            b.cfg.inbox.as_ref().unwrap().triage.as_ref().unwrap(),
            crate::now(),
        );
        let used = view
            .slots
            .iter()
            .filter(|s| {
                s.kind == crate::snapshot::SlotKind::Drain
                    && s.state != crate::snapshot::SlotState::Idle
            })
            .count();
        assert_eq!(
            used,
            if blocked { 3 } else { 0 },
            "{state}: {:?}",
            view.slots
        );
        if blocked {
            let holders: Vec<_> = view
                .slots
                .iter()
                .filter_map(|s| s.holder.as_ref()?.job.clone())
                .collect();
            assert!(holders.contains(&"drained-1".to_owned()), "{holders:?}");
        }
        b.pass().drain().unwrap();
        assert_eq!(
            b.log().contains("issues/4/labels -f labels[]=claude"),
            !blocked,
            "{state}: {}",
            b.log()
        );
    }
}

#[test]
fn full_drain_only_fetches_cached_repair_candidates() {
    let b = Box::new();
    let state = &b.cfg.inbox.as_ref().unwrap().state;
    let busy = state.join("sam_game-99");
    fs::create_dir_all(&busy).unwrap();
    fs::write(busy.join(files::File::Drained), "sam/game#99\n").unwrap();
    fs::write(busy.join(files::File::Job), "busy\n").unwrap();
    let job = crate::job::Job::new(&b.cfg, "busy");
    fs::create_dir_all(&job.dir).unwrap();
    fs::write(job.path("turns"), "1").unwrap();
    fs::write(job.turn("1", "outcome"), "running").unwrap();
    fs::write(job.turn("1", "slot"), "1").unwrap();
    fs::write(b.path("active"), "").unwrap();
    let old = "2026-10-01T00:00:00Z";
    let mut queued: Vec<_> = (4..10)
        .map(|n| found("sam/game", n, "sam", &["ready"], old))
        .collect();
    b.pass().note_ready("sam/game", &queued).unwrap();
    let before = b.log();
    b.pass().drain().unwrap();
    assert_eq!(
        b.log(),
        before,
        "a full drain must not fetch ordinary candidates"
    );
    let repair_label = b.cfg.inbox.as_ref().unwrap().unstick_label.clone();
    let repair = found("sam/game", 10, "sam", &["ready", &repair_label], old);
    queued.push(repair.clone());
    b.pass().note_ready("sam/game", &queued).unwrap();
    // Cached repair status only permits the refresh, not admission: losing
    // the live label must still respect ordinary capacity.
    let mut ordinary_now = repair.clone();
    ordinary_now["labels"] = json!([{"name":"ready"}]);
    on_github(&b, &ordinary_now);
    b.pass().drain().unwrap();
    assert!(!b.log().contains("labels[]="), "{}", b.log());
    on_github(&b, &repair);
    b.pass().drain().unwrap();
    let log = b.log();
    for n in 4..10 {
        assert!(
            !log.contains(&format!("api repos/sam/game/issues/{n}")),
            "{log}"
        );
    }
    assert!(log.contains("api repos/sam/game/issues/10"), "{log}");
    assert!(log.contains("issues/10/labels -f labels[]=claude"), "{log}");
}

mod admission;

// A subscription turn retried after its usage limit keeps its place while
// the plan's hold keeps it; the credit's room isn't held by it, as the line
// isn't. With no hold on it, it goes first.
#[test]
fn a_plan_held_retry_leaves_the_api_credits_room() {
    let renews = jiff::Timestamp::now()
        .to_zoned_datetime(jiff::tz::TimeZone::UTC)
        .date()
        .checked_add(jiff::Span::new().days(2))
        .unwrap();
    let mut b = Box::new();
    let dir = b.path("settings");
    crate::config::tests::set(&mut b.cfg, &dir, &[&format!("credit 20 {renews} 200 5")]);
    let cfg = &b.cfg;
    fs::create_dir_all(&cfg.jobs).unwrap();
    let job = crate::job::Job::new(cfg, "retried");
    fs::create_dir_all(&job.dir).unwrap();
    crate::policy::bind(
        &job,
        crate::policy::Identity::Local,
        crate::policy::Kind::Manual,
    )
    .unwrap();
    job.set_turns(1).unwrap();
    crate::record::mark_turn(&job, "1", crate::record::TurnMarker::RetryMigration)
        .swap(
            &json!({"waited": crate::now_nanos(), "said": "spent", "deadline": null,
                    "name": "2-1-retried", "first": false, "retained": true})
            .to_string(),
        )
        .unwrap();
    assert_eq!(crate::retry::admission(cfg).unwrap().len(), 1);
    let usage = cfg.jobs.join(crate::usage::FILE);
    fs::write(
        &usage,
        json!({"at":crate::now(), "until":crate::now() + 3600, "said":"spent", "job":"retried"})
            .to_string(),
    )
    .unwrap();
    assert!(crate::slot::seen_room(cfg, Engine::ClaudeApi, false));
    assert!(!crate::slot::seen_room(cfg, Engine::Claude, false));
    fs::remove_file(&usage).unwrap();
    assert!(!crate::slot::seen_room(cfg, Engine::ClaudeApi, false));
}
