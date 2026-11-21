use super::*;

/// An account whose meter reads from a list, and whose resets answer
/// from another, keeping before each yes the way Claude Code's driver
/// does.
struct Fake {
    reads: Vec<Result<Reading, String>>,
    resets: Vec<Reset>,
    asked: usize,
    /// What another reader last fetched, and when, for each read that fails.
    copies: Vec<Option<(f64, Reading)>>,
}

impl Account for Fake {
    fn read(&mut self) -> Result<Reading, String> {
        self.reads.remove(0)
    }
    fn copy(&mut self) -> Option<(f64, Reading)> {
        (!self.copies.is_empty())
            .then(|| self.copies.remove(0))
            .flatten()
    }
    fn reset(&mut self, keep: &mut dyn FnMut() -> Result<(), String>) -> Reset {
        self.asked += 1;
        match self.resets.remove(0) {
            Reset::Pressed(said) => match keep() {
                Ok(()) => Reset::Pressed(said),
                Err(e) => Reset::NotPressed(e),
            },
            other => other,
        }
    }
}

fn fake(reads: Vec<Result<Reading, String>>, resets: Vec<Reset>) -> Fake {
    Fake {
        reads,
        resets,
        asked: 0,
        copies: vec![],
    }
}

/// The week the tests run in, and when it ends by itself; the passes
/// run near the start of time, long before.
const WEEK: &str = "2026-11-04T19:00";
const ENDS: f64 = 1_793_818_800.0;

fn at(used: f64) -> Reading {
    Reading {
        used,
        week: WEEK.to_owned(),
        ends: ENDS,
        session: None,
    }
}

/// A reading in another week, ending at ENDS.
fn week_of(week: &str, ends: f64, used: f64) -> Reading {
    Reading {
        used,
        week: week.to_owned(),
        ends,
        session: None,
    }
}

fn keys(notices: &[Notice]) -> Vec<String> {
    notices
        .iter()
        .map(|n| match n {
            Notice::Open(k, ..) => format!("open {k}"),
            Notice::Close(k, _) => format!("close {k}"),
        })
        .collect()
}

fn run(a: &mut Fake, state: State, now: f64) -> Outcome {
    pass(a, &mut |_| Ok(()), state, 2.0, now, "Thu 9:41 PM PDT")
}

#[test]
fn plenty_left_presses_nothing() {
    let mut a = fake(vec![Ok(at(40.0))], vec![]);
    let o = run(&mut a, State::default(), 100.0);
    assert_eq!(a.asked, 0);
    assert_eq!(keys(&o.notices), ["close usage-reset-failed"]);
    assert_eq!(o.state.last, Some((100.0, 40.0)));
    assert_eq!(o.state.week.as_deref(), Some("2026-11-04T19:00"));
    assert_eq!(o.wake, 100.0 + LATEST);
}

#[test]
fn under_the_line_presses_once_and_says_so() {
    // A reset that moved the week's end.
    let after = week_of("2026-11-06T05:00", 1_793_941_200.0, 0.0);
    let mut a = fake(
        vec![Ok(at(98.4)), Ok(after)],
        vec![Reset::Pressed("Weekly reset used · 0 left".to_owned())],
    );
    let o = run(&mut a, State::default(), 100.0);
    assert_eq!(a.asked, 1);
    assert!(o.state.pressed);
    assert_eq!(
        keys(&o.notices),
        [
            "open usage-reset",
            "close usage-reset",
            "close usage-reset-failed"
        ]
    );
    let Notice::Open(_, title, body) = &o.notices[0] else {
        panic!()
    };
    assert_eq!(title, "Used the banked weekly Claude reset at 98% used");
    assert!(
        body.contains("read 98% used") && body.contains("reads 0% now") && body.contains("0 left"),
        "{body}"
    );
    assert_eq!(o.state.week.as_deref(), Some("2026-11-06T05:00"));
    assert_eq!(o.state.last, Some((100.0, 0.0)));
    assert_eq!(o.wake, 100.0 + LATEST);
}

#[test]
fn exactly_at_the_line_counts_and_so_does_the_limit() {
    let mut a = fake(
        vec![Ok(at(98.0)), Ok(at(0.0))],
        vec![Reset::Pressed(String::new())],
    );
    run(&mut a, State::default(), 100.0);
    let mut b = fake(
        vec![Ok(at(100.0)), Ok(at(0.0))],
        vec![Reset::Pressed(String::new())],
    );
    run(&mut b, State::default(), 100.0);
    assert_eq!((a.asked, b.asked), (1, 1));
}

#[test]
fn a_week_is_never_reset_twice() {
    let mut a = fake(
        vec![Ok(at(99.0)), Ok(at(99.0))],
        vec![Reset::Pressed("?".to_owned())],
    );
    let o = run(&mut a, State::default(), 100.0);
    assert_eq!(keys(&o.notices), ["open usage-reset-failed"]);
    let Notice::Open(_, _, body) = &o.notices[0] else {
        panic!()
    };
    assert!(body.contains("still reads 99%"), "{body}");
    let mut a = fake(vec![Ok(at(100.0))], vec![]);
    let o = run(&mut a, o.state, 200.0);
    assert_eq!(a.asked, 0);
    assert!(o.notices.is_empty());
    assert_eq!(o.wake, 200.0 + LATEST);
}

#[test]
fn the_week_is_kept_as_spent_before_its_yes() {
    let mut kept = Vec::new();
    let mut a = fake(
        vec![Ok(at(99.0)), Ok(at(0.0))],
        vec![Reset::Pressed(String::new())],
    );
    let o = pass(
        &mut a,
        &mut |s: &State| {
            kept.push(s.clone());
            Ok(())
        },
        State::default(),
        2.0,
        100.0,
        "Thu 9:41 PM PDT",
    );
    assert_eq!(kept.len(), 1);
    assert!(kept[0].pressed);
    assert_eq!(
        kept[0].unsettled,
        Some(Unsettled {
            used: 99.0,
            at: "Thu 9:41 PM PDT".to_owned(),
            week: WEEK.to_owned(),
            ends: ENDS,
            said: None,
        })
    );
    // Settled by the pass that pressed it.
    assert!(o.state.pressed && o.state.unsettled.is_none());

    // Nothing kept, nothing pressed: a try that can be made again.
    let mut a = fake(vec![Ok(at(99.0))], vec![Reset::Pressed(String::new())]);
    let o = pass(
        &mut a,
        &mut |_| Err("disk full".to_owned()),
        State::default(),
        2.0,
        100.0,
        "",
    );
    assert!(!o.state.pressed && o.state.unsettled.is_none());
    assert_eq!(o.state.failed_tries, 1);
}

/// What a pass cut off after its yes kept: the week spent, and the
/// reading it was confirmed at.
fn kept(used: f64) -> State {
    State {
        week: Some(WEEK.to_owned()),
        ends: Some(ENDS),
        pressed: true,
        unsettled: Some(Unsettled {
            used,
            at: "Thu 9:41 PM PDT".to_owned(),
            week: WEEK.to_owned(),
            ends: ENDS,
            said: None,
        }),
        ..State::default()
    }
}

#[test]
fn a_pass_cut_off_after_its_yes_is_settled_by_the_next() {
    // The meter dropped, its week's end where it was.
    let mut a = fake(vec![Ok(at(3.0))], vec![]);
    let o = run(&mut a, kept(99.0), 200.0);
    assert_eq!(a.asked, 0);
    assert_eq!(
        keys(&o.notices),
        [
            "open usage-reset",
            "close usage-reset",
            "close usage-reset-failed"
        ]
    );
    let Notice::Open(_, title, body) = &o.notices[0] else {
        panic!()
    };
    assert!(title.starts_with("Used"), "{title}");
    assert!(
        body.contains("cut off") && body.contains("reads 3% now"),
        "{body}"
    );
    assert!(o.state.pressed && o.state.unsettled.is_none());

    // Its week's end moved: the reset's doing, and the moved week is
    // the spent one.
    let moved = || week_of(MOVED, MOVED_ENDS, 99.5);
    let mut a = fake(vec![Ok(moved())], vec![]);
    let o = run(&mut a, kept(99.0), 200.0);
    assert_eq!(keys(&o.notices)[0], "open usage-reset");
    assert_eq!(o.state.week.as_deref(), Some("2026-11-06T05:00"));
    let mut a = fake(vec![Ok(moved())], vec![]);
    run(&mut a, o.state, 300.0);
    assert_eq!(a.asked, 0);

    // It didn't take: the owner hears, and it's never pressed again.
    let mut a = fake(vec![Ok(at(99.5))], vec![]);
    let o = run(&mut a, kept(99.0), 200.0);
    assert_eq!(a.asked, 0);
    assert_eq!(keys(&o.notices), ["open usage-reset-failed"]);
    let mut a = fake(vec![Ok(at(99.8))], vec![]);
    let o = run(&mut a, o.state, 300.0);
    assert_eq!(a.asked, 0);
    assert!(o.notices.is_empty());

    // A meter it can't read leaves it for the pass after.
    let mut a = fake(vec![Err("offline".to_owned())], vec![]);
    let o = run(&mut a, kept(99.0), 200.0);
    assert!(o.state.unsettled.is_some() && o.state.pressed);
}

#[test]
fn a_meter_unread_after_the_yes_leaves_the_week_spent() {
    let mut a = fake(
        vec![Ok(at(99.0)), Err("offline".to_owned())],
        vec![Reset::Pressed("Weekly limit refilled".to_owned())],
    );
    let o = run(&mut a, State::default(), 100.0);
    assert!(o.notices.is_empty());
    assert!(o.state.pressed);
    assert_eq!(
        o.state.unsettled.as_ref().and_then(|u| u.said.as_deref()),
        Some("Weekly limit refilled")
    );
    assert_eq!(o.wake, 100.0 + RETRY);
    // The next reading is in a week the reset moved: that week is the
    // spent one, and the owner hears what Claude Code said.
    let moved = || week_of(MOVED, MOVED_ENDS, 99.0);
    let mut a = fake(vec![Ok(moved())], vec![]);
    let o = run(&mut a, o.state, 400.0);
    assert_eq!(a.asked, 0);
    let Notice::Open(_, _, body) = &o.notices[0] else {
        panic!()
    };
    assert!(body.contains("Weekly limit refilled"), "{body}");
    let mut a = fake(vec![Ok(moved())], vec![]);
    run(&mut a, o.state, 500.0);
    assert_eq!(a.asked, 0);
}

/// The week the fixtures' reset moves the end to, and the one after it.
const MOVED: &str = "2026-11-06T05:00";
const MOVED_ENDS: f64 = 1_793_941_200.0;

#[test]
fn a_moved_end_stays_spent_past_the_old_end() {
    // Settled after the old end, in the week the reset moved it to:
    // that week is the spent one.
    let mut a = fake(vec![Ok(week_of(MOVED, MOVED_ENDS, 99.0))], vec![]);
    let o = run(&mut a, kept(99.0), ENDS + 60.0);
    assert_eq!(a.asked, 0);
    let Notice::Open(_, title, _) = &o.notices[0] else {
        panic!()
    };
    assert!(title.starts_with("Used"), "{title}");
    assert!(o.state.pressed);
    let mut a = fake(vec![Ok(week_of(MOVED, MOVED_ENDS, 99.5))], vec![]);
    let o = run(&mut a, o.state, ENDS + 120.0);
    assert_eq!(a.asked, 0);
    // A week on from the moved end is a new week.
    let next = || week_of("2026-11-13T05:00", MOVED_ENDS + 7.0 * 86400.0, 99.0);
    let mut a = fake(
        vec![
            Ok(next()),
            Ok(week_of("2026-11-13T05:00", MOVED_ENDS + 7.0 * 86400.0, 0.0)),
        ],
        vec![Reset::Pressed(String::new())],
    );
    run(&mut a, o.state, MOVED_ENDS + 60.0);
    assert_eq!(a.asked, 1);

    // A spent week, settled, whose end moves later: still spent.
    let spent = State {
        week: Some(WEEK.to_owned()),
        ends: Some(ENDS),
        pressed: true,
        ..State::default()
    };
    let mut a = fake(vec![Ok(week_of(MOVED, MOVED_ENDS, 99.5))], vec![]);
    let o = run(&mut a, spent, 100.0);
    assert_eq!(a.asked, 0);
    assert!(o.state.pressed);
    assert_eq!(o.state.week.as_deref(), Some(MOVED));
}

#[test]
fn a_pass_across_the_weeks_end_leaves_the_next_unspent() {
    // Confirmed in the old week, read after in the new one: the drop is
    // the new week's.
    let next = |used| week_of("2026-11-11T19:00", 1_794_423_600.0, used);
    let mut a = fake(
        vec![Ok(at(99.0)), Ok(next(2.0))],
        vec![Reset::Pressed("Weekly limit refilled".to_owned())],
    );
    let o = run(&mut a, State::default(), 100.0);
    let Notice::Open(_, title, body) = &o.notices[0] else {
        panic!()
    };
    assert!(title.starts_with("Pressed"), "{title}");
    assert!(body.contains("ended by itself"), "{body}");
    assert!(!o.state.pressed && o.state.unsettled.is_none());
    let mut a = fake(
        vec![Ok(next(99.0)), Ok(next(0.0))],
        vec![Reset::Pressed(String::new())],
    );
    run(&mut a, o.state, 200.0);
    assert_eq!(a.asked, 1);
}

#[test]
fn a_week_that_ended_by_itself_leaves_the_next_unspent() {
    // One week on, or two: the meter's drop is the new week's, not the
    // reset's, and the new week can use one.
    for (week, ends) in [
        ("2026-11-11T19:00", 1_794_423_600.0),
        ("2026-11-18T19:00", 1_795_028_400.0),
    ] {
        let mut a = fake(vec![Ok(week_of(week, ends, 4.0))], vec![]);
        let o = run(&mut a, kept(99.0), ENDS + 60.0);
        assert_eq!(a.asked, 0);
        assert_eq!(
            keys(&o.notices)[..2],
            ["open usage-reset", "close usage-reset"]
        );
        let Notice::Open(_, title, body) = &o.notices[0] else {
            panic!()
        };
        assert!(title.starts_with("Pressed"), "{title}");
        assert!(body.contains("ended by itself"), "{body}");
        assert!(!o.state.pressed && o.state.unsettled.is_none());
        assert_eq!(o.state.week.as_deref(), Some(week));
        let mut a = fake(
            vec![Ok(week_of(week, ends, 99.0)), Ok(week_of(week, ends, 0.0))],
            vec![Reset::Pressed(String::new())],
        );
        run(&mut a, o.state, ENDS + 3600.0);
        assert_eq!(a.asked, 1);
    }
}

#[test]
fn a_new_week_starts_over() {
    let state = State {
        week: Some("2026-10-28T19:00".to_owned()),
        ends: Some(ENDS - 7.0 * 86400.0),
        pressed: true,
        last: Some((0.0, 99.0)),
        ..State::default()
    };
    let mut a = fake(
        vec![Ok(at(98.5)), Ok(at(1.0))],
        vec![Reset::Pressed(String::new())],
    );
    run(&mut a, state, 100.0);
    assert_eq!(a.asked, 1);
}

#[test]
fn nothing_banked_asks_again_hours_later() {
    let mut a = fake(
        vec![Ok(at(99.9))],
        vec![Reset::NoneBanked("No weekly reset to use".to_owned())],
    );
    let o = run(&mut a, State::default(), 100.0);
    assert!(o.notices.is_empty());
    assert_eq!(o.state.none_at, Some(100.0));
    assert_eq!(o.wake, 100.0 + LATEST);
    let mut a = fake(vec![Ok(at(99.9))], vec![]);
    let o = run(&mut a, o.state, 100.0 + LATEST);
    assert_eq!(a.asked, 0);
    let mut a = fake(vec![Ok(at(99.9))], vec![Reset::NoneBanked(String::new())]);
    run(&mut a, o.state, 100.0 + NONE_AGAIN);
    assert_eq!(a.asked, 1);
}

#[test]
fn a_reset_for_the_limit_waits_for_it() {
    let mut a = fake(
        vec![Ok(at(98.5))],
        vec![Reset::AtLimitOnly("at a limit".to_owned())],
    );
    let o = run(&mut a, State::default(), 0.0);
    assert!(o.state.at_limit_only && o.notices.is_empty());
    // Looked at by when the week would be used up, not by the line.
    assert!((o.wake - 450.0).abs() < 1e-6, "{}", o.wake);
    let mut a = fake(vec![Ok(at(99.5))], vec![]);
    let o = run(&mut a, o.state, 500.0);
    assert_eq!(a.asked, 0);
    let mut a = fake(
        vec![Ok(at(100.0)), Ok(at(0.0))],
        vec![Reset::Pressed(String::new())],
    );
    let o = run(&mut a, o.state, 1000.0);
    assert_eq!(a.asked, 1);
    assert_eq!(keys(&o.notices)[0], "open usage-reset");
}

#[test]
fn tries_that_confirm_nothing_stop_after_three_and_say_so() {
    let mut state = State::default();
    for i in 1..=4 {
        let resets = if i <= 3 {
            vec![Reset::NotPressed("no dialog".to_owned())]
        } else {
            vec![]
        };
        let now = 100.0 * i as f64;
        let mut a = fake(vec![Ok(at(99.0))], resets);
        let o = run(&mut a, state, now);
        assert_eq!(a.asked, usize::from(i <= 3));
        assert_eq!(
            keys(&o.notices),
            if i == 3 {
                vec!["open usage-reset-failed".to_owned()]
            } else {
                vec![]
            }
        );
        // A try left comes soon, before the week runs out; once they're
        // spent, the owner has the notice and nothing's left to try.
        assert_eq!(o.wake, now + if i < 3 { RETRY } else { LATEST });
        state = o.state;
    }
}

#[test]
fn blind_after_three_and_clears_on_the_next_read() {
    let mut state = State::default();
    for i in 1..=4 {
        let mut a = fake(vec![Err("not logged in".to_owned())], vec![]);
        let o = run(&mut a, state, 100.0);
        assert_eq!(o.wake, 100.0 + RETRY);
        assert_eq!(
            keys(&o.notices),
            if i == 3 {
                vec!["open usage-reset-blind".to_owned()]
            } else {
                vec![]
            }
        );
        state = o.state;
    }
    let mut a = fake(vec![Ok(at(10.0))], vec![]);
    let o = run(&mut a, state, 100.0);
    assert_eq!(
        keys(&o.notices),
        ["close usage-reset-blind", "close usage-reset-failed"]
    );
    assert_eq!(o.state.failed_reads, 0);
}

#[test]
fn looks_sooner_as_the_week_runs_out() {
    // 10 points of room at the assumed 6 an hour: 100 minutes, so 50.
    let mut a = fake(vec![Ok(at(88.0))], vec![]);
    assert_eq!(run(&mut a, State::default(), 0.0).wake, 3000.0);
    // 1 point: 10 minutes, so 5.
    let mut a = fake(vec![Ok(at(97.0))], vec![]);
    assert_eq!(run(&mut a, State::default(), 0.0).wake, 300.0);
    // Never sooner than SOONEST.
    let mut a = fake(vec![Ok(at(97.9))], vec![]);
    assert_eq!(run(&mut a, State::default(), 0.0).wake, SOONEST);
}

#[test]
fn a_fast_burn_brings_the_next_look_in() {
    // 2 points in 10 minutes is 12 an hour: 10 points of room is 50
    // minutes, so 25.
    let state = State {
        week: Some(at(0.0).week),
        last: Some((0.0, 86.0)),
        ..State::default()
    };
    let mut a = fake(vec![Ok(at(88.0))], vec![]);
    assert_eq!(run(&mut a, state, 600.0).wake, 600.0 + 1500.0);
}

/// A pass at NOW whose own read fails as remi's did when the endpoint
/// refused, with COPY what another session last fetched.
fn refused(state: State, copy: Option<(f64, Reading)>, now: f64) -> Outcome {
    let mut a = fake(
        vec![Err("its usage endpoint not answering".to_owned())],
        vec![],
    );
    a.copies = vec![copy];
    run(&mut a, state, now)
}

/// The state after a good reading of USED at AT.
fn read_at(at: f64, used: f64) -> State {
    let mut a = fake(vec![Ok(self::at(used))], vec![]);
    run(&mut a, State::default(), at).state
}

#[test]
fn a_refused_read_takes_another_sessions_newer_copy() {
    // remi#1516: three refusals in a row, while other sessions' copies,
    // 461s, 72s and 63s old, showed the week at 56-57%. Not blind.
    let mut state = read_at(10_000.0, 56.0);
    for (now, age, used) in [
        (11_800.0, 461.0, 56.0),
        (12_100.0, 72.0, 56.0),
        (12_400.0, 63.0, 57.0),
    ] {
        let o = refused(state, Some((now - age, at(used))), now);
        assert!(keys(&o.notices).iter().all(|k| !k.starts_with("open")));
        assert_eq!(o.state.failed_reads, 0);
        // Its burn and next look count from when the copy was fetched.
        assert_eq!(o.state.last, Some((now - age, used)));
        state = o.state;
    }
    // A fresh reading is tried again no later than after any failed one,
    // and sooner if the copy's room runs out first: 1 point at the assumed
    // burn is 10 minutes, so its next look is 5 minutes off the copy's
    // time.
    assert_eq!(
        refused(state, Some((12_500.0, at(57.0))), 12_600.0).wake,
        12_600.0 + RETRY
    );
    let o = refused(
        read_at(10_000.0, 96.0),
        Some((12_500.0, at(97.0))),
        12_600.0,
    );
    assert_eq!(o.state.failed_reads, 0);
    assert_eq!(o.wake, 12_500.0 + 300.0);
}

#[test]
fn a_copy_never_presses_or_settles_a_reset() {
    // Over the line: only a fresh reading may press.
    let o = refused(
        read_at(10_000.0, 97.0),
        Some((10_200.0, at(99.0))),
        10_300.0,
    );
    assert_eq!(o.state.failed_reads, 1);
    assert!(!o.state.pressed);
    // A press still unsettled waits for a fresh reading, however new the
    // copy: one fetched just before the yes would say it didn't take.
    let mut state = read_at(10_000.0, 97.0);
    state.unsettled = Some(Unsettled {
        used: 99.0,
        at: "then".to_owned(),
        week: WEEK.to_owned(),
        ends: ENDS,
        said: None,
    });
    state.pressed = true;
    let o = refused(state, Some((10_200.0, at(3.0))), 10_300.0);
    assert!(o.state.unsettled.is_some());
    assert_eq!(o.state.failed_reads, 1);
    assert!(o.notices.is_empty());
}

#[test]
fn a_copy_counts_only_while_it_says_more_than_the_passes_know() {
    // No older than the last reading, nor dated ahead of the clock.
    let o = refused(read_at(10_000.0, 56.0), Some((9_900.0, at(56.0))), 10_300.0);
    assert_eq!(o.state.failed_reads, 1);
    let o = refused(
        read_at(10_000.0, 56.0),
        Some((10_400.0, at(56.0))),
        10_300.0,
    );
    assert_eq!(o.state.failed_reads, 1);
    // With nothing read yet, there's no telling how far the week has gone.
    let o = refused(State::default(), Some((10_200.0, at(56.0))), 10_300.0);
    assert_eq!(o.state.failed_reads, 1);
    // Three points short of the line at the assumed burn: half an hour, so
    // a copy 15 minutes old or more may be over it by now.
    let state = read_at(5_000.0, 90.0);
    let o = refused(state.clone(), Some((10_000.0, at(95.0))), 10_000.0 + 900.0);
    assert_eq!(o.state.failed_reads, 1);
    let o = refused(state, Some((10_000.0, at(95.0))), 10_000.0 + 899.0);
    assert_eq!(o.state.failed_reads, 0);
    // No copy at all: a failed read, as before.
    let o = refused(read_at(10_000.0, 56.0), None, 10_300.0);
    assert_eq!(o.state.failed_reads, 1);
}
