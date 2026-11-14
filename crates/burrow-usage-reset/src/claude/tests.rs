use super::*;
use crate::pass::{Notice, Outcome, RETRY, State, pass};
use serde_json::json;

/// Its screen once it's up, as remi's Claude Code 2.1.314 drew it
/// (remi#208), its notices above the input box.
const UP: &str = "
 ▐▛███▛█   Claude Code v2.1.314
▝▜██████▀  Opus 5.5 · Claude Max
 ▝▝   ▝▝   /var/lib/burrow/usage-reset/claude

  Get to finished work sooner with Opus 5.5. Switch anytime with /model.
  /limit-reset to use your weekly reset · 1 left · use by Nov 6
  Weekly reset kept · run /limit-reset at a limit to use it · use by Nov 6

                                You've used 71% of your weekly limit · resets Nov 4, 12pm (America/Los_Angeles)
────────────────────────────────────────────────────────────────────────────────
❯ Try \"create a util logging.py that...\"
────────────────────────────────────────────────────────────────────────────────
  ⏵⏵ bypass permissions on (shift+tab to cycle) · ← for agents
";

/// The screen once COMMAND is in and answered with LINES.
fn answered(lines: &str) -> String {
    let top = UP.split("────").next().unwrap();
    format!("{top}❯ /limit-reset weekly\n{lines}\n────────\n❯ \n────────\n")
}

#[test]
fn its_input_box() {
    assert!(prompt_ready(UP));
    assert!(!asks_trust(UP));
    // Claude Code 2.1.314's, as remi's tmux showed it: its no first, and
    // the cursor on it.
    let trust = "
 Accessing workspace:

 /var/lib/remi-update/usage-reset/claude

 Quick safety check: Is this a project you created or one you trust? (Like your own code, a well-known open source project, or work from your team). If not, take a moment to review what's in this folder first.

 Claude Code'll be able to read, edit, and execute files here.

 Security guide

 ❯ No, exit
   Yes, I trust this folder

 Enter to confirm · Esc to cancel
";
    assert!(asks_trust(trust) && !prompt_ready(trust));
    assert!(asks_trust(
        &trust
            .replace("❯ No, exit", "  No, exit")
            .replace("  Yes, I trust", "❯ Yes, I trust")
    ));
}

#[test]
fn only_its_answer_counts() {
    // The notices it started with (one says "at a limit to use it")
    // settle nothing: not before COMMAND is typed, nor with it still in
    // the box, nor in a frame drawn after Enter and before the echo.
    assert_eq!(read_reset(UP), Screen::Busy);
    let typed = UP.replace(
        "❯ Try \"create a util logging.py that...\"",
        "❯ /limit-reset weekly",
    );
    assert_eq!(read_reset(&typed), Screen::Busy);
    assert_eq!(
        read_reset(&format!("{UP}❯ /limit-reset weekly\n")),
        Screen::Busy
    );
    assert_eq!(
        read_reset(&answered("  ⎿  Checking for a reset…")),
        Screen::Busy
    );
}

#[test]
fn its_confirmation_and_where_the_cursor_is() {
    let dialog = |yes: &str, no: &str| {
        answered(&format!(
            " Use a weekly reset?\n Refills your weekly limit now · your weekly reset day stays Wednesday\n 0 weekly resets left · use by Nov 6\n\n {yes}1. Yes, use my weekly reset\n {no}2. No, keep it\n"
        ))
    };
    assert_eq!(
        read_reset(&dialog("❯ ", "  ")),
        Screen::Confirm(Cursor::OnYes)
    );
    assert_eq!(
        read_reset(&dialog("  ", "❯ ")),
        Screen::Confirm(Cursor::Below)
    );
    assert_eq!(
        read_reset(&dialog("  ", "  ")),
        Screen::Confirm(Cursor::Above)
    );
    // Drawn over the transcript, its echo gone.
    assert_eq!(
        read_reset(" Use a weekly reset?\n\n ❯ Yes, use my weekly reset\n   No, keep it\n"),
        Screen::Confirm(Cursor::OnYes)
    );
}

#[test]
fn its_lines_that_use_nothing() {
    let said = |line: &str| read_reset(&answered(&format!("  ⎿  {line}")));
    assert_eq!(
        said("No reset to use"),
        Screen::Said(Reset::NoneBanked("No reset to use".to_owned()))
    );
    assert!(matches!(
        said("No weekly reset to use · this limit resets Nov 4"),
        Screen::Said(Reset::NoneBanked(_))
    ));
    assert!(matches!(
        said(
            "Your reset only works at a usage limit, and you're not at one now · nothing was used"
        ),
        Screen::Said(Reset::AtLimitOnly(_))
    ));
    assert!(matches!(
        said(
            "You have a reset saved for when you reach a usage limit · run /limit-reset when you do"
        ),
        Screen::Said(Reset::AtLimitOnly(_))
    ));
    assert!(matches!(
        said("Couldn't reset your limits · nothing was used · try again in a moment"),
        Screen::Said(Reset::NotPressed(_))
    ));
    assert!(matches!(
        said(
            "Your weekly reset can't be used right now · usable once you reach a limit it refills · you can use nothing here"
        ),
        Screen::Said(Reset::AtLimitOnly(_))
    ));
    assert!(matches!(
        said("None of your resets refill your weekly limit"),
        Screen::Said(Reset::NotPressed(_))
    ));
    // A Claude Code without it isn't an empty bank.
    assert!(matches!(
        said("Unknown command: /limit-reset"),
        Screen::Said(Reset::NotPressed(_))
    ));
    assert!(matches!(
        said("Unknown slash command: limit-reset"),
        Screen::Said(Reset::NotPressed(_))
    ));
}

#[test]
fn its_last_word_after_the_yes() {
    let dialog = " Use a weekly reset?\n Refills your weekly limit now\n\n ❯ Yes, use my weekly reset\n   No, keep it\n";
    let top = UP.split("────").next().unwrap();
    let up = format!("{top}❯ /limit-reset weekly\n{dialog}");
    // Still asking: the press wasn't taken.
    assert!(still_asking(&up) && finished(&up).is_none());
    // Taken, and going through.
    let working = format!(
        "{top}❯ /limit-reset weekly\n Use a weekly reset?\n Resetting your limits… this usually takes a few seconds\n"
    );
    assert!(!still_asking(&working) && finished(&working).is_none());
    // The same with its input box drawn under it.
    let boxed = format!("{working}────────\n❯ \n────────\n");
    assert!(finished(&boxed).is_none());
    // Back at its input box, with nothing under the echo yet.
    assert!(finished(&answered("")).is_none());
    let done = answered(
        "  ⎿  Weekly limit refilled · your weekly reset day stays Wednesday · 0 weekly resets left",
    );
    assert!(!still_asking(&done));
    assert_eq!(
        finished(&done).as_deref(),
        Some(
            "Weekly limit refilled · your weekly reset day stays Wednesday · 0 weekly resets left"
        )
    );
    let unsure =
        answered("  ⎿  Couldn't confirm the reset went through · check /usage in a moment");
    assert!(finished(&unsure).is_some());
}

/// An answer as Claude Code's endpoint gave it, 2.1.316's fields.
fn fetched(used: f64) -> Value {
    json!({
        "session": {"total_cost_usd": 0},
        "subscription_type": "max",
        "rate_limits_available": true,
        "rate_limits": {
            "five_hour": {"utilization": 35.0, "resets_at": "2026-10-30T06:40:00.412Z"},
            "seven_day": {"utilization": used, "resets_at": "2026-11-04T19:00:00.412Z"},
            "limits": [{"kind": "weekly_all", "group": "weekly", "percent": used}]
        },
        "behaviors": null
    })
}

/// The same, answered from its persisted copy with its endpoint refusing.
fn from_the_copy(used: f64) -> Value {
    let mut answer = fetched(used);
    answer["rate_limits"]
        .as_object_mut()
        .unwrap()
        .remove("limits");
    answer
}

const NOW: f64 = 1_793_000_000.0;

#[test]
fn the_meter() {
    assert_eq!(
        usage(&fetched(71.4), None, NOW),
        Ok(Reading {
            used: 71.4,
            week: "2026-11-04T19:00".to_owned(),
            ends: 1_793_818_800.0,
            session: Some(Window {
                used: 35.0,
                resets_at: 1_793_342_400.0,
            }),
        })
    );
    // No session running: no five-hour window, and still a reading.
    let mut idle = fetched(71.4);
    idle["rate_limits"]["five_hour"] = Value::Null;
    assert_eq!(usage(&idle, None, NOW).unwrap().session, None);
    let mut broken = fetched(71.4);
    broken["rate_limits"]["five_hour"] = json!({"utilization": "lots"});
    assert!(usage(&broken, None, NOW).is_err());
    // A copy too old to answer from: its endpoint was asked.
    assert!(usage(&fetched(71.4), Some(NOW - SNAPSHOT), NOW).is_ok());
    // Logged in with an API key, or not at all.
    let none =
        json!({"subscription_type": null, "rate_limits_available": false, "rate_limits": null});
    assert!(usage(&none, None, NOW).unwrap_err().contains("logged in"));
}

/// `read_fresh` where the copy's time reads from COPIES in turn, the clock
/// goes on by a minute with every look, and asking is noted.
fn asked_through(copies: Vec<Result<Option<f64>, String>>, start: f64) -> Result<Reading, String> {
    let mut copies = copies.into_iter();
    let mut clock = start;
    let mut asked = false;
    let out = read_fresh(
        &mut || copies.next().unwrap(),
        &mut || {
            clock += 60.0;
            clock - 60.0
        },
        &mut || {
            asked = true;
            Ok(fetched(95.0))
        },
    );
    out.inspect(|_| assert!(asked))
}

#[test]
fn freshness_holds_across_the_asking() {
    let copy = Some(NOW - 59.0);
    // Under SNAPSHOT when asked, past it by the time the answer's in.
    let e = asked_through(vec![Ok(copy), Ok(copy)], NOW).unwrap_err();
    assert!(e.contains("snapshot 59s"), "{e}");
    // Dated ahead of the clock, which then may catch up with it before
    // Claude Code looks.
    let ahead = Some(NOW + 0.5);
    let e = asked_through(vec![Ok(ahead), Ok(ahead)], NOW).unwrap_err();
    assert!(e.contains("snapshot"), "{e}");
    // Past it when asked: it can't have been what answered.
    let old = Some(NOW - 60.0);
    assert!(asked_through(vec![Ok(old), Ok(old)], NOW).is_ok());
    // Nothing there, before or after: its endpoint answered.
    assert!(asked_through(vec![Ok(None), Ok(None)], NOW).is_ok());
    // Replaced while it was asked: that copy may be the answer.
    for after in [Some(NOW + 1.0), None] {
        let e = asked_through(vec![Ok(old), Ok(after)], NOW).unwrap_err();
        assert!(e.contains("changed"), "{e}");
    }
    let e = asked_through(vec![Ok(None), Ok(Some(NOW + 1.0))], NOW).unwrap_err();
    assert!(e.contains("changed"), "{e}");
    // Not known, before or after.
    let unknown = || Err("unreadable".to_owned());
    assert!(asked_through(vec![unknown()], NOW).is_err());
    assert!(asked_through(vec![Ok(old), unknown()], NOW).is_err());
}

#[test]
fn only_a_config_showing_no_copy_counts_as_none() {
    assert_eq!(copied_in(b"{}"), Ok(None));
    assert_eq!(
        copied_in(br#"{"cachedUsageUtilization":{"fetchedAtMs":5000}}"#),
        Ok(Some(5.0))
    );
    assert!(copied_in(b"{\"cachedUsage").is_err());
    assert!(copied_in(br#"{"cachedUsageUtilization":{}}"#).is_err());
}

#[test]
fn a_cached_answer_is_no_reading() {
    let e = usage(&from_the_copy(95.0), Some(NOW - 3000.0), NOW).unwrap_err();
    assert!(e.contains("cached") && e.contains("3000s"), "{e}");
    assert!(usage(&from_the_copy(95.0), None, NOW).is_err());
    let e = usage(&fetched(95.0), Some(NOW - 10.0), NOW).unwrap_err();
    assert!(e.contains("snapshot 10s"), "{e}");
}

/// An account whose meter is `get_usage` answers, asked at NOW.
struct Answers {
    answers: Vec<(Value, Option<f64>)>,
    pressed: usize,
}

impl Account for Answers {
    fn read(&mut self) -> Result<Reading, String> {
        let (answer, copied) = self.answers.remove(0);
        usage(&answer, copied, NOW)
    }
    fn reset(&mut self, keep: &mut dyn FnMut() -> Result<(), String>) -> Reset {
        self.pressed += 1;
        keep().unwrap();
        Reset::Pressed("Weekly limit refilled".to_owned())
    }
}

fn pass_over(answers: Vec<(Value, Option<f64>)>, state: State) -> (Outcome, usize) {
    let mut account = Answers {
        answers,
        pressed: 0,
    };
    let o = pass(&mut account, &mut |_| Ok(()), state, 2.0, NOW, "now");
    (o, account.pressed)
}

#[test]
fn a_pass_doesnt_trust_a_cached_answer() {
    // Past the line, but as the meter stood up to an hour ago: the week
    // it's from may be over or already refilled. Nothing's pressed.
    let (o, pressed) = pass_over(
        vec![(from_the_copy(99.0), Some(NOW - 3000.0))],
        State::default(),
    );
    assert_eq!(pressed, 0);
    assert!(!o.state.pressed);
    assert_eq!(o.state.failed_reads, 1);

    // Short of the line: its next look isn't booked by a meter this old,
    // nor does its burn count. A fresh 95 with this much burn would be
    // booked later, off the room it seems to have.
    let last = Some((NOW - 3600.0, 90.0));
    let state = State {
        last,
        ..State::default()
    };
    let (o, _) = pass_over(vec![(from_the_copy(95.0), Some(NOW - 3000.0))], state);
    assert_eq!(o.wake, NOW + RETRY);
    assert_eq!(o.state.last, last);
    assert!(o.state.week.is_none());

    // Fresh again, the same meter is believed.
    let (o, pressed) = pass_over(
        vec![(fetched(99.0), Some(NOW - 3000.0)), (fetched(0.0), None)],
        o.state,
    );
    assert_eq!(pressed, 1);
    assert_eq!(o.state.failed_reads, 0);
}

#[test]
fn a_snapshot_from_before_the_yes_doesnt_settle_it() {
    // A copy written while the reset's Claude Code came up, still under
    // SNAPSHOT at the read after its yes, shows the meter from before it.
    let (o, pressed) = pass_over(
        vec![(fetched(99.0), None), (fetched(99.0), Some(NOW - 20.0))],
        State::default(),
    );
    assert_eq!(pressed, 1);
    assert!(keys_of(&o.notices).is_empty(), "{:?}", o.notices);
    assert!(o.state.pressed && o.state.unsettled.is_some());
    assert_eq!(o.wake, NOW + RETRY);
    // The next look, on a fresh answer, sees it took.
    let (o, pressed) = pass_over(vec![(fetched(3.0), None)], o.state);
    assert_eq!(pressed, 0);
    assert_eq!(keys_of(&o.notices)[0], "usage-reset");
    assert!(o.state.unsettled.is_none());
}

fn keys_of(notices: &[Notice]) -> Vec<&'static str> {
    notices
        .iter()
        .filter_map(|n| match n {
            Notice::Open(k, ..) => Some(*k),
            Notice::Close(..) => None,
        })
        .collect()
}
