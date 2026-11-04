//! When a usage limit resets, by Claude Code's own words for it.
//!
//! Claude Code ends a run it can't make with its own words for why, which
//! say when the limit resets, in the zone it shows times in: "You've hit
//! your session limit · resets 3pm (America/Los_Angeles)", with the date
//! once that's more than a day off ("You've hit your monthly spend limit ·
//! raise it at … · your weekly limit resets Nov 4, 12pm
//! (America/Los_Angeles)"), and the year too once it isn't this one.
//! [`resets`] reads that back, for a refusal whose stream doesn't carry the
//! reset as a timestamp ([`crate::usage::refused`]).

use jiff::Timestamp;
use jiff::civil::{Date, Time};
use jiff::tz::TimeZone;

/// How long ago a reset said as a time alone may have passed and still be
/// that one, in seconds: the turn's end, and proj reading it, lag behind
/// the words.
const FLOOR: i64 = 900;

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// When the usage limit `text` says a run hit resets, in seconds since the
/// epoch, read at `now`: None when it names no limit, or no reset that can
/// be read for sure (a zone this machine doesn't know, say).
pub fn resets(text: &str, now: i64) -> Option<i64> {
    if !["You've hit your", "You've reached your"]
        .iter()
        .any(|s| text.contains(s))
    {
        return None;
    }
    let (_, after) = text.split_once(" resets ")?;
    let (when, rest) = after.split_once(" (")?;
    let (zone, _) = rest.split_once(')')?;
    let tz = TimeZone::get(zone).ok()?;
    let today = Timestamp::from_second(now).ok()?.to_zoned(tz.clone());
    // "Nov 4, 2027, 9am", "Nov 4, 12pm" or "3:30pm".
    let (date, clock) = match when.rsplit_once(", ") {
        Some((date, clock)) => (Some(date), clock),
        None => (None, when),
    };
    let time = time_of(clock)?;
    let at = match date {
        Some(date) => {
            let (month_day, year) = match date.split_once(", ") {
                Some((md, y)) => (md, y.parse().ok()?),
                None => (date, today.year()),
            };
            let (month, day) = month_day.split_once(' ')?;
            let month = MONTHS.iter().position(|m| *m == month)? as i8 + 1;
            Date::new(year, month, day.parse().ok()?).ok()?
        }
        // A time alone is within the day: the next time the clock says it,
        // or the last one if that passed no more than FLOOR ago. The turn's
        // end and this reading lag behind the words, a moment or across
        // midnight; a later one read as passed costs one more refusal FLOOR
        // on, which then reads as the next.
        None => {
            let date = today.date();
            [date.yesterday().ok()?, date, date.tomorrow().ok()?]
                .into_iter()
                .find(|d| {
                    d.to_datetime(time)
                        .to_zoned(tz.clone())
                        .is_ok_and(|at| at.timestamp().as_second() >= now - FLOOR)
                })?
        }
    };
    let at = at.to_datetime(time).to_zoned(tz).ok()?;
    Some(at.timestamp().as_second())
}

/// "12pm", "3:30am": the time of day it names.
fn time_of(clock: &str) -> Option<Time> {
    let (number, half) = if let Some(n) = clock.strip_suffix("am") {
        (n, 0)
    } else {
        (clock.strip_suffix("pm")?, 12)
    };
    let (hour, minute) = number.split_once(':').unwrap_or((number, "0"));
    let hour: i8 = hour.parse().ok().filter(|h| (1..=12).contains(h))?;
    let minute: i8 = minute.parse().ok()?;
    Time::new(hour % 12 + half, minute, 0, 0).ok()
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;

    use super::resets;

    fn at(s: &str) -> i64 {
        s.parse::<Timestamp>().unwrap().as_second()
    }

    // The words a job on remi ended on, 2026-11-02 02:47 UTC: noon Pacific on
    // 11-04 is 20:00 UTC (standard time from 11-01).
    #[test]
    fn a_reset_with_its_date() {
        let said = "You've hit your monthly spend limit · raise it at https://claude.ai/settings/usage?from=cc_cli_limit_message · your weekly limit resets Nov 4, 12pm (America/Los_Angeles)";
        let now = at("2026-11-02T02:47:45Z");
        assert_eq!(resets(said, now), Some(at("2026-11-04T20:00:00Z")));
    }

    // A time alone is the next one: later today, or tomorrow once it's past.
    #[test]
    fn a_reset_within_the_day() {
        let said = "You've hit your session limit · resets 3:30pm (UTC)";
        assert_eq!(
            resets(said, at("2026-11-04T09:00:00Z")),
            Some(at("2026-11-04T15:30:00Z"))
        );
        assert_eq!(
            resets(said, at("2026-11-04T16:00:00Z")),
            Some(at("2026-11-05T15:30:00Z"))
        );
        assert_eq!(
            resets(
                "You've hit your weekly limit · resets 12am (UTC)",
                at("2026-11-04T23:00:00Z")
            ),
            Some(at("2026-11-05T00:00:00Z"))
        );
    }

    // One that passed no more than FLOOR ago is that one, not tomorrow's: the
    // turn's end, and proj reading it, lag behind the words. Across midnight
    // too.
    #[test]
    fn a_reset_just_past() {
        let said = "You've hit your session limit · resets 3:30pm (UTC)";
        for now in [
            "2026-11-04T15:30:00Z",
            "2026-11-04T15:30:05Z",
            "2026-11-04T15:45:00Z",
        ] {
            assert_eq!(
                resets(said, at(now)),
                Some(at("2026-11-04T15:30:00Z")),
                "{now}"
            );
        }
        assert_eq!(
            resets(said, at("2026-11-04T15:45:01Z")),
            Some(at("2026-11-05T15:30:00Z"))
        );
        let late = "You've hit your weekly limit · resets 11:55pm (UTC)";
        assert_eq!(
            resets(late, at("2026-11-05T00:05:00Z")),
            Some(at("2026-11-04T23:55:00Z"))
        );
        assert_eq!(
            resets(
                "You've hit your weekly limit · resets 12am (UTC)",
                at("2026-11-05T00:00:30Z")
            ),
            Some(at("2026-11-05T00:00:00Z"))
        );
    }

    // Past this year's end, the year is said; the zone's own offset counts.
    #[test]
    fn a_reset_next_year() {
        let said = "You've reached your weekly limit · resets Jan 2, 2027, 9am (Europe/Berlin)";
        assert_eq!(
            resets(said, at("2026-12-30T12:00:00Z")),
            Some(at("2027-01-02T08:00:00Z"))
        );
    }

    // No limit named, no reset, a zone or time that can't be read: none.
    #[test]
    fn no_reset_it_can_read() {
        let now = at("2026-11-04T09:00:00Z");
        for said in [
            "The build failed: see the log, which resets 3pm (UTC) logs",
            "You've hit your monthly spend limit.",
            "You've hit your weekly limit · resets 3pm (Mars/Olympus_Mons)",
            "You've hit your weekly limit · resets 13pm (UTC)",
            "You've hit your weekly limit · resets Smarch 4, 12pm (UTC)",
            "You've hit your weekly limit · resets soon (UTC)",
            "You've hit your weekly limit · resets 3pm",
            "",
        ] {
            assert_eq!(resets(said, now), None, "{said}");
        }
    }
}
