//! One pass: read the weekly meter, use the banked reset if the week is
//! nearly gone, and say when to look next. Everything here is decided from
//! what the account says and what the passes kept; Claude Code, the state
//! file and the owner's notices are the caller's.

use serde::{Deserialize, Serialize};

/// The soonest the next pass comes: the meter can't move far in this.
pub const SOONEST: f64 = 120.0;
/// The latest: the hourly catch-up passes over it anyway.
pub const LATEST: f64 = 3600.0;
/// After a pass that couldn't read the meter.
pub const RETRY: f64 = 300.0;
/// The burn assumed until two readings show a faster one, in percent of the
/// week a second (6 an hour): busy, so a week near its end is looked at
/// often.
const ASSUMED_BURN: f64 = 6.0 / 3600.0;
/// How long a week that had nothing banked waits before it's asked again,
/// in case one turns up.
const NONE_AGAIN: f64 = 6.0 * 3600.0;
/// Failed reads in a row before the owner hears the meter can't be read,
/// and failed tries in a week before the watcher stops trying it.
const TRIES: u32 = 3;

/// The account's weekly meter, as a pass reads it.
#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    /// Percent of the week's limit used, 0 to 100.
    pub used: f64,
    /// When the week's meter resets by itself, as the account says it: the
    /// week's name, so a pass knows a new week from the last one.
    pub week: String,
}

/// What asking for the banked reset came to.
#[derive(Debug, Clone, PartialEq)]
pub enum Reset {
    /// Its yes was pressed, or may have been: the meter says whether it
    /// took. What Claude Code said after.
    Pressed(String),
    /// Nothing is banked for the weekly limit.
    NoneBanked(String),
    /// One is banked, but it can only be used once the week is used up.
    AtLimitOnly(String),
    /// Nothing was confirmed, for this reason: safe to try again.
    NotPressed(String),
}

/// What the account can be asked: the meter, and to use its banked reset.
pub trait Account {
    fn read(&mut self) -> Result<Reading, String>;
    /// Ask for the banked reset, calling KEEP once just before its yes is
    /// first pressed. If KEEP fails, nothing is pressed.
    fn reset(&mut self, keep: &mut dyn FnMut() -> Result<(), String>) -> Reset;
}

/// What the passes keep from one to the next.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct State {
    /// The last good reading: when, and how much of the week was used.
    pub last: Option<(f64, f64)>,
    /// The week everything below is about. A new one starts them over.
    pub week: Option<String>,
    /// A reset was confirmed this week: never another, even if the meter
    /// didn't show it take.
    pub pressed: bool,
    /// When this week was last found to have nothing banked.
    pub none_at: Option<f64>,
    /// This week's reset can only be used at the limit.
    pub at_limit_only: bool,
    /// Tries this week that confirmed nothing.
    pub failed_tries: u32,
    /// Reads that failed in a row.
    pub failed_reads: u32,
    /// A reset whose yes was about to be pressed, kept before it was: the
    /// pass that pressed it settles it, or the next one does if that pass
    /// was cut off first.
    pub unsettled: Option<Unsettled>,
}

/// A reset about to be confirmed: the meter then, and when, as the owner's
/// clock says it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Unsettled {
    pub used: f64,
    pub at: String,
}

/// Something the owner hears, as HOST-notify keys it.
#[derive(Debug, Clone, PartialEq)]
pub enum Notice {
    /// A notice, with its title and body.
    Open(&'static str, String, String),
    /// The problem a notice was about cleared, saying how.
    Close(&'static str, String),
}

/// The reset went through: opened and closed at once, as the record of it.
pub const USED: &str = "usage-reset";
/// The reset was due and didn't happen, or didn't take.
pub const FAILED: &str = "usage-reset-failed";
/// The meter can't be read.
pub const BLIND: &str = "usage-reset-blind";

pub struct Outcome {
    pub state: State,
    pub notices: Vec<Notice>,
    /// When the next pass should run, in seconds since the epoch.
    pub wake: f64,
}

/// A pass at NOW (STAMP, as the owner's clock says it), with the reset due
/// once the week has less than BELOW percent left. KEEP saves a state for
/// good before the reset's yes is pressed, so a pass cut off after pressing
/// it can't leave the next one free to press another.
pub fn pass(
    account: &mut impl Account,
    keep: &mut impl FnMut(&State) -> Result<(), String>,
    mut state: State,
    below: f64,
    now: f64,
    stamp: &str,
) -> Outcome {
    let mut notices = Vec::new();
    let reading = match account.read() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("couldn't read the meter: {e}");
            state.failed_reads += 1;
            if state.failed_reads == TRIES {
                notices.push(Notice::Open(
                    BLIND,
                    "Can't read the Claude usage meter".to_owned(),
                    format!(
                        "The usage watcher has failed {TRIES} times in a row to read the weekly meter through Claude Code, so it can't use the banked reset when the week runs low. The last try said: {e}"
                    ),
                ));
            }
            return Outcome {
                state,
                notices,
                wake: now + RETRY,
            };
        }
    };
    if state.failed_reads >= TRIES {
        notices.push(Notice::Close(
            BLIND,
            "The usage watcher reads the meter again.".to_owned(),
        ));
    }
    state.failed_reads = 0;
    if let Some(then) = state.unsettled.clone() {
        // The pass that pressed it didn't see how it went: this reading
        // says.
        settle(&mut state, &mut notices, &then, None, Ok(reading.clone()));
    }
    if state.week.as_deref() != Some(&reading.week) {
        // A new week: what the last one found is no news about this one, and
        // its burn isn't this one's.
        state = State {
            week: Some(reading.week.clone()),
            ..State::default()
        };
    }

    let limit = if state.at_limit_only {
        100.0
    } else {
        100.0 - below
    };
    let due = reading.used >= limit
        && !state.pressed
        && state.failed_tries < TRIES
        && state.none_at.is_none_or(|at| now - at >= NONE_AGAIN);
    let mut reading = reading;
    if due {
        let then = Unsettled {
            used: reading.used,
            at: stamp.to_owned(),
        };
        let reserved = State {
            pressed: true,
            unsettled: Some(then.clone()),
            ..state.clone()
        };
        match account.reset(&mut || keep(&reserved)) {
            Reset::Pressed(said) => {
                let after = account.read();
                if let Some(r) = settle(&mut state, &mut notices, &then, Some(&said), after) {
                    reading = r;
                }
            }
            Reset::NoneBanked(said) => {
                eprintln!("nothing banked: {said}");
                state.none_at = Some(now);
            }
            Reset::AtLimitOnly(said) if reading.used < 100.0 => {
                eprintln!("banked, for the limit only: {said}");
                state.at_limit_only = true;
            }
            Reset::AtLimitOnly(said) | Reset::NotPressed(said) => {
                state.failed_tries += 1;
                eprintln!(
                    "the reset wasn't confirmed ({} of {TRIES}): {said}",
                    state.failed_tries
                );
                if state.failed_tries == TRIES {
                    notices.push(Notice::Open(
                        FAILED,
                        "Couldn't use the banked Claude reset".to_owned(),
                        format!(
                            "The week's meter reads {:.0}% used, but {TRIES} tries at the banked reset confirmed nothing, so the usage watcher has stopped trying this week. Claude Code said, last: \"{said}\". Run /limit-reset in Claude Code to use it by hand.",
                            reading.used
                        ),
                    ));
                }
            }
        }
    }
    // Room again, after a reset or without one: a failure the owner heard
    // of is over, unless this pass just had one.
    let failed = notices
        .iter()
        .any(|n| matches!(n, Notice::Open(FAILED, ..)));
    if reading.used < 100.0 - below && !failed {
        notices.push(Notice::Close(FAILED, "The week has room again.".to_owned()));
    }

    let wake = now
        + next(
            &state,
            &reading,
            if state.at_limit_only {
                100.0
            } else {
                100.0 - below
            },
            now,
        );
    state.last = Some((now, reading.used));
    Outcome {
        state,
        notices,
        wake,
    }
}

/// How a reset confirmed at THEN went, from the meter AFTER: it took if the
/// meter dropped. Either way the week's reset is spent, and the owner hears
/// which. SAID is what Claude Code answered, if the pass lived to see it.
/// The reading it took with, if it did.
fn settle(
    state: &mut State,
    notices: &mut Vec<Notice>,
    then: &Unsettled,
    said: Option<&str>,
    after: Result<Reading, String>,
) -> Option<Reading> {
    state.pressed = true;
    state.unsettled = None;
    let answer = match said {
        Some(said) => format!("Claude Code: \"{said}\""),
        None => "the pass that did was cut off before Claude Code answered".to_owned(),
    };
    match after {
        Ok(r) if r.used < then.used => {
            notices.push(Notice::Open(
                USED,
                format!("Used the banked weekly Claude reset at {:.0}% used", then.used),
                format!(
                    "At {} the week's meter read {:.0}% used, so the usage watcher used the banked reset ({answer}). It reads {:.0}% now.",
                    then.at, then.used, r.used
                ),
            ));
            notices.push(Notice::Close(
                USED,
                "Nothing to do: that's the record.".to_owned(),
            ));
            // The reset may have started a new week, or not; the burn
            // before it isn't the burn after.
            state.week = Some(r.week.clone());
            state.last = None;
            Some(r)
        }
        after => {
            notices.push(Notice::Open(
                FAILED,
                "The banked Claude reset didn't take".to_owned(),
                format!(
                    "At {} the week's meter read {:.0}% used and the usage watcher confirmed the banked reset ({answer}), but the meter {}. It won't press it again this week; run /limit-reset in Claude Code to look.",
                    then.at,
                    then.used,
                    match after {
                        Ok(r) => format!("still reads {:.0}%", r.used),
                        Err(e) => format!("couldn't be read after: {e}"),
                    }
                ),
            ));
            None
        }
    }
}

/// How long until the next pass: half the time the meter would take to reach
/// LIMIT at the faster of the burn since the last pass and ASSUMED_BURN.
fn next(state: &State, reading: &Reading, limit: f64, now: f64) -> f64 {
    let room = limit - reading.used;
    if room <= 0.0 {
        // Over it with nothing to do (none banked, this week's used, or
        // tried out): a new week, or a reset showing up, is what changes it.
        return LATEST;
    }
    let seen = match state.last {
        Some((at, used)) if now > at && reading.used > used => (reading.used - used) / (now - at),
        _ => 0.0,
    };
    (room / seen.max(ASSUMED_BURN) / 2.0).clamp(SOONEST, LATEST)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An account whose meter reads from a list, and whose resets answer
    /// from another, keeping before each yes the way Claude Code's driver
    /// does.
    struct Fake {
        reads: Vec<Result<Reading, String>>,
        resets: Vec<Reset>,
        asked: usize,
    }

    impl Account for Fake {
        fn read(&mut self) -> Result<Reading, String> {
            self.reads.remove(0)
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
        }
    }

    fn at(used: f64) -> Reading {
        Reading {
            used,
            week: "2026-11-04T19:00".to_owned(),
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
        let after = Reading {
            used: 0.0,
            week: "2026-11-06T05:00".to_owned(),
        };
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
            body.contains("read 98% used")
                && body.contains("reads 0% now")
                && body.contains("0 left"),
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
                at: "Thu 9:41 PM PDT".to_owned()
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

    #[test]
    fn a_pass_cut_off_after_its_yes_is_settled_by_the_next() {
        // What the cut-off pass kept, and the meter the next one reads.
        let kept = |used| State {
            week: Some(at(0.0).week),
            pressed: true,
            unsettled: Some(Unsettled {
                used,
                at: "Thu 9:41 PM PDT".to_owned(),
            }),
            ..State::default()
        };
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
        let Notice::Open(_, _, body) = &o.notices[0] else {
            panic!()
        };
        assert!(
            body.contains("cut off") && body.contains("reads 3% now"),
            "{body}"
        );
        assert!(o.state.pressed && o.state.unsettled.is_none());

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
    fn a_new_week_starts_over() {
        let state = State {
            week: Some("2026-10-28T19:00".to_owned()),
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
            let mut a = fake(vec![Ok(at(99.0))], resets);
            let o = run(&mut a, state, 100.0 * i as f64);
            assert_eq!(a.asked, usize::from(i <= 3));
            assert_eq!(
                keys(&o.notices),
                if i == 3 {
                    vec!["open usage-reset-failed".to_owned()]
                } else {
                    vec![]
                }
            );
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
}
