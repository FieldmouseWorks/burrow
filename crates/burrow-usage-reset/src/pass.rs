//! One pass: read the weekly meter, use the banked reset if the week is
//! nearly gone, and say when to look next. Everything here is decided from
//! what the account says and what the passes kept; Claude Code, the state
//! file and the owner's notices are the caller's.

use serde::{Deserialize, Serialize};

/// The soonest the next pass comes: the meter can't move far in this.
pub const SOONEST: f64 = 120.0;
/// The latest: the hourly catch-up passes over it anyway.
pub const LATEST: f64 = 3600.0;
/// After a pass that couldn't read the meter, or whose try at the reset
/// confirmed nothing.
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
/// How far a week's end has to move on for the meter to be in another week:
/// weeks end a week apart, give or take a clock change, and anything nearer
/// is the same week's end moved.
const ROLLOVER: f64 = 6.0 * 86400.0;

/// The account's weekly meter, as a pass reads it.
#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    /// Percent of the week's limit used, 0 to 100.
    pub used: f64,
    /// When the week's meter resets by itself, as the account says it: the
    /// week's name, so a pass knows a new week from the last one.
    pub week: String,
    /// The same moment, in seconds since the epoch.
    pub ends: f64,
    /// The five-hour session's window, while one runs: routing's, not the
    /// reset's.
    pub session: Option<crate::meter::Window>,
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
    /// The meter as it stands now. A copy of it from earlier, however it
    /// looks, is an error: a pass presses and settles only on a fresh
    /// reading, and only a copy from [`Account::copy`] stands in for one.
    fn read(&mut self) -> Result<Reading, String>;
    /// The meter as another reader of the same account last fetched it
    /// from its endpoint, and when, if one is kept: a real reading, only an
    /// older one. Nothing when there's none, or it can't be told to be that.
    fn copy(&mut self) -> Option<(f64, Reading)>;
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
    /// The week everything below is about, and when it ends by itself. A
    /// new one starts them over.
    pub week: Option<String>,
    pub ends: Option<f64>,
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

/// A reset about to be confirmed, and the reading it was confirmed at.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Unsettled {
    /// Percent of the week used then.
    pub used: f64,
    /// When, as the owner's clock says it.
    pub at: String,
    /// The week it was for, and when that week was to end by itself.
    pub week: String,
    pub ends: f64,
    /// What Claude Code answered its yes with, once a pass saw it.
    pub said: Option<String>,
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
    // Other readers of the account share its endpoint's rate limit, so it
    // often refuses, or isn't asked for a minute after one read it: what
    // the last of them read stands in then, when it can.
    let read = account.read().map(|r| (now, r, false)).or_else(|e| {
        account
            .copy()
            .filter(|(at, copy)| shows_room(&state, *at, copy, below, now))
            .map(|(at, copy)| (at, copy, true))
            .ok_or(e)
    });
    let (mut read_at, reading, copied) = match read {
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
        settle(&mut state, &mut notices, &then, &reading);
    }
    if state.week.as_deref() != Some(&reading.week) {
        if state.pressed && state.ends.is_some_and(|ends| !rolled_over(ends, &reading)) {
            // The spent week's end moved, the reset's doing: still the
            // spent week.
            state.week = Some(reading.week.clone());
            state.ends = Some(reading.ends);
            state.last = None;
        } else {
            // A new week: what the last one found is no news about this
            // one, and its burn isn't this one's.
            state = fresh(&reading);
        }
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
    // A try that confirmed nothing, with tries left: the next comes soon,
    // while the week still has room.
    let mut again = false;
    if due {
        let mut then = Unsettled {
            used: reading.used,
            at: stamp.to_owned(),
            week: reading.week.clone(),
            ends: reading.ends,
            said: None,
        };
        let reserved = State {
            pressed: true,
            unsettled: Some(then.clone()),
            ..state.clone()
        };
        match account.reset(&mut || keep(&reserved)) {
            Reset::Pressed(said) => {
                then.said = Some(said);
                state.pressed = true;
                match account.read() {
                    Ok(r) => {
                        settle(&mut state, &mut notices, &then, &r);
                        reading = r;
                        read_at = now;
                    }
                    Err(e) => {
                        // Spent, and settled by the next pass that can read
                        // the meter.
                        eprintln!("couldn't read the meter after the reset: {e}");
                        state.unsettled = Some(then);
                    }
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
                again = state.failed_tries < TRIES;
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

    // The reading's own time, not the pass's, says how far the week can
    // have gone since: a copy's is earlier. A copy stands in for a reading
    // that failed, so a fresh one is tried no later than after any failure,
    // for proj's meter too.
    let wake = if state.unsettled.is_some() || again {
        now + RETRY
    } else {
        let limit = if state.at_limit_only {
            100.0
        } else {
            100.0 - below
        };
        let booked = (read_at + next(&state, &reading, limit, read_at)).max(now + SOONEST);
        if copied {
            booked.min(now + RETRY)
        } else {
            booked
        }
    };
    state.last = Some((read_at, reading.used));
    Outcome {
        state,
        notices,
        wake,
    }
}

/// Whether a copy of the meter fetched at AT can stand for a reading at NOW:
/// only when all it can say is that the week still has room. It's never
/// what a reset is pressed or settled on: one over the line, or a press
/// still unsettled, waits for a fresh reading. It has to be newer than the
/// last reading the passes made (so newer than any press, whose pass
/// reads after it), and young enough that, at the burn the passes assume, the
/// week can't have reached the line since it was fetched: the pass that
/// read it then would have looked again no sooner than now.
fn shows_room(state: &State, at: f64, copy: &Reading, below: f64, now: f64) -> bool {
    let limit = 100.0 - below;
    state.unsettled.is_none()
        && copy.used < limit
        && at <= now
        && state.last.is_some_and(|(last, _)| at > last)
        && now < at + next(state, copy, limit, at)
}

/// The meter is in a later week than the one that was to end at ENDS: it
/// has rolled over by itself since, once or more.
fn rolled_over(ends: f64, r: &Reading) -> bool {
    r.ends >= ends + ROLLOVER
}

/// What the passes keep, starting R's week.
fn fresh(r: &Reading) -> State {
    State {
        week: Some(r.week.clone()),
        ends: Some(r.ends),
        ..State::default()
    }
}

/// How a reset confirmed at THEN went, from the reading R, and the owner
/// hears it. If THEN's week has rolled over by itself since, that week is
/// over whatever the reset did, and R's starts unspent. If not, a meter that
/// dropped or a week's end that moved is the reset's doing: it took, and
/// R's week is the spent one. One that didn't didn't take, and the week is
/// spent all the same.
fn settle(state: &mut State, notices: &mut Vec<Notice>, then: &Unsettled, r: &Reading) {
    state.unsettled = None;
    let answer = match &then.said {
        Some(said) => format!("Claude Code: \"{said}\""),
        None => "the pass that did was cut off before Claude Code answered".to_owned(),
    };
    if rolled_over(then.ends, r) {
        notices.push(Notice::Open(
            USED,
            format!("Pressed the banked weekly Claude reset at {:.0}% used", then.used),
            format!(
                "At {} the week's meter read {:.0}% used and the usage watcher confirmed the banked reset ({answer}), but the week ended by itself before it saw whether the reset took. The new week's meter reads {:.0}%; run /limit-reset in Claude Code to see what's left.",
                then.at, then.used, r.used
            ),
        ));
        notices.push(Notice::Close(
            USED,
            "Nothing to do: that's the record.".to_owned(),
        ));
        *state = fresh(r);
    } else if r.used < then.used || r.week != then.week {
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
        // The reset may have moved the week's end, or not; the burn before
        // it isn't the burn after.
        state.pressed = true;
        state.week = Some(r.week.clone());
        state.ends = Some(r.ends);
        state.last = None;
    } else {
        notices.push(Notice::Open(
            FAILED,
            "The banked Claude reset didn't take".to_owned(),
            format!(
                "At {} the week's meter read {:.0}% used and the usage watcher confirmed the banked reset ({answer}), but the meter still reads {:.0}%. It won't press it again this week; run /limit-reset in Claude Code to look.",
                then.at, then.used, r.used
            ),
        ));
        state.pressed = true;
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
mod tests;
