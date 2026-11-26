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

/// Claude Code 2.1.330's debug log of a `get_usage` its endpoint refused
/// with STATUS, as remi's showed a 429.
fn refused_with(status: u16) -> String {
    format!(
        "2026-11-21T22:47:06.725Z [DEBUG] fetchUtilization: GET /api/oauth/usage (attempt 1)
2026-11-21T22:47:06.784Z [DEBUG] fetchUtilization: {status} remembered for this bearer; not asking again for 300s
2026-11-21T22:47:06.785Z [ERROR] Failed to load usage data: Request failed with status code {status}
"
    )
}

#[test]
fn the_endpoints_answers_in_its_log() {
    assert_eq!(usage_statuses(&refused_with(429)), [429, 429]);
    assert_eq!(
        usage_statuses(
            "x [DEBUG] fetchUtilization: GET /api/oauth/usage (attempt 1)\n\
             x [DEBUG] fetchUtilization: 200 after 1 attempt(s)\n\
             x [DEBUG] [Bootstrap] Fetch ok, 404 of them\n"
        ),
        [200]
    );
    assert_eq!(usage_statuses(""), [] as [u16; 0]);
}

#[test]
fn a_rate_limit_isnt_a_failed_login() {
    let failed = || usage(&from_the_copy(10.0), None, NOW).map(|_| ());
    assert!(failed().is_err());
    // The endpoint's rate limit: the login works.
    assert_eq!(
        login_shown(failed(), &from_the_copy(10.0), &refused_with(429)),
        Ok(())
    );
    // Even with no copy to answer from.
    let mut nothing = from_the_copy(10.0);
    nothing["rate_limits"]["seven_day"] = Value::Null;
    assert_eq!(
        login_shown(Err("x".into()), &nothing, &refused_with(429)),
        Ok(())
    );
    // A refusal of the login, or nothing in the log to say why: its error.
    let why = failed().unwrap_err();
    let refused = Err(Unshown::Unknown(format!(
        "{why} (its usage endpoint: Request failed with status code 401)"
    )));
    assert_eq!(
        login_shown(failed(), &from_the_copy(10.0), &refused_with(401)),
        refused
    );
    assert_eq!(
        login_shown(failed(), &from_the_copy(10.0), ""),
        Err(Unshown::Unknown(why.clone()))
    );
    let both = refused_with(429) + &refused_with(401);
    assert_eq!(login_shown(failed(), &from_the_copy(10.0), &both), refused);
    // No plan limits, or no answer at all, fails whatever the log says.
    let none =
        json!({"subscription_type": null, "rate_limits_available": false, "rate_limits": null});
    assert!(login_shown(Err("x".into()), &none, &refused_with(429)).is_err());
    assert!(login_shown(Err("x".into()), &Value::Null, &refused_with(429)).is_err());
    // A reading is the proof.
    assert_eq!(login_shown(Ok(()), &fetched(10.0), ""), Ok(()));
}

#[test]
fn only_an_answer_not_from_the_endpoint_waits_for_the_log() {
    // One from the endpoint is killed at once, before it writes its copy.
    assert!(!unfetched(&fetched(10.0)));
    assert!(!unfetched(&json!({"rate_limits_available": false})));
    assert!(unfetched(&from_the_copy(10.0)));
    assert!(unfetched(
        &json!({"rate_limits_available": true, "rate_limits": null})
    ));
}

#[test]
fn only_a_failed_login_asks_for_a_new_one() {
    // As 2.1.330 logged and answered each against a stand-in endpoint.
    let none =
        json!({"subscription_type": null, "rate_limits_available": false, "rate_limits": null});
    let unread = json!({"rate_limits_available": true, "rate_limits": null});
    let shown = |answer: &Value, log: &str| login_shown(Err("x".into()), answer, log);
    let login = |answer: &Value, log: &str| {
        let shown = shown(answer, log);
        assert!(matches!(shown, Err(Unshown::LoginFailed(_))), "{shown:?}");
    };
    let refresh = |how: &str| format!("2026 [ERROR] OAuth refresh failed (expected): {how}\n");
    let refresh_refused = refresh("Request failed with status code 400");
    // Logged out, or cleared when its refresh was refused.
    login(&none, "");
    login(&none, &refresh_refused);
    // Revoked: the endpoint refused it, then the token endpoint its refresh.
    for status in [401, 403] {
        login(&unread, &(refresh_refused.clone() + &refused_with(status)));
    }

    let unknown = |answer: &Value, log: &str, says: &str| match shown(answer, log) {
        Err(Unshown::Unknown(why)) => assert!(why.contains(says), "{why}"),
        other => panic!("{other:?}"),
    };
    // Refused with no refresh refused: it didn't get through, or wasn't tried.
    for how in [
        "connect ECONNREFUSED 127.0.0.1:9",
        "Request failed with status code 503",
    ] {
        unknown(
            &unread,
            &(refresh(how) + &refused_with(401)),
            "status code 401",
        );
    }
    unknown(&unread, &refused_with(403), "status code 403");
    // Another session's refresh won the race, and then the retry failed.
    let retried = refresh_refused.clone()
        + &refused_with(401)
        + "2026 [ERROR] Failed to load usage data: timeout of 5000ms exceeded\n";
    unknown(&unread, &retried, "timeout of 5000ms exceeded");
    // A 5xx: the status, without the body beside it.
    let down = "2026 [ERROR] \"AxiosError: [url=/api/oauth/usage,status=500,body=private] x\"\n";
    assert_eq!(
        shown(&unread, down),
        Err(Unshown::Unknown(
            "x (its usage endpoint: Request failed with status code 500)".into()
        ))
    );
    // No answer: whatever the log holds, it isn't this asking's.
    let log = refresh_refused + &refused_with(401);
    assert_eq!(shown(&Value::Null, &log), Err(Unshown::Unknown("x".into())));
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
        &mut |_| {
            asked = true;
            Ok(fetched(95.0))
        },
    );
    out.inspect(|_| assert!(asked)).map_err(Unread::said)
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

/// `login_within` on a clock from NOW that only waiting and asking move: the
/// copy is FIRST until another session writes one at each of REWRITES
/// (seconds in), and each asking takes ASKING seconds and gets ANSWER, with
/// no debug log. What it says, and the seconds in at which it asked.
fn login_through(
    first: Option<f64>,
    rewrites: &[f64],
    answer: Value,
) -> (Result<Option<Checked>, Unshown>, Vec<f64>) {
    login_logged(first, rewrites, answer, "")
}

/// login_through, each asking leaving LOG as Claude Code's debug log.
fn login_logged(
    first: Option<f64>,
    rewrites: &[f64],
    answer: Value,
    log: &str,
) -> (Result<Option<Checked>, Unshown>, Vec<f64>) {
    const ASKING: f64 = 2.0;
    let t = std::cell::Cell::new(0.0);
    let copy = || {
        rewrites
            .iter()
            .rev()
            .find(|&&at| at <= t.get())
            .map(|at| NOW + at)
            .or(first)
    };
    let mut asked = Vec::new();
    let out = login_within(
        LOGIN_BUDGET,
        &mut || Ok(copy()),
        &mut || NOW + t.get(),
        &mut || Duration::from_secs_f64(t.get()),
        &mut |wait| t.set(t.get() + wait.as_secs_f64()),
        &mut |left, _| {
            assert!(left >= ASK_LEAST, "{left:?}");
            asked.push(t.get());
            t.set(t.get() + ASKING);
            Ok(answer.clone())
        },
        &mut || log.to_owned(),
    );
    assert!(t.get() <= LOGIN_BUDGET.as_secs_f64(), "{}", t.get());
    (out, asked)
}

#[test]
fn another_sessions_copy_is_waited_out_in_turn() {
    let ok_at = |at: f64| {
        Ok(Some(Checked {
            requested_at: NOW + at,
            reading: usage(&fetched(5.0), None, NOW).unwrap(),
        }))
    };
    // No copy, or one too old to answer: asked at once.
    assert_eq!(
        login_through(None, &[], fetched(5.0)),
        (ok_at(0.0), vec![0.0])
    );
    assert_eq!(
        login_through(Some(NOW - 90.0), &[], fetched(5.0)),
        (ok_at(0.0), vec![0.0])
    );
    // A copy 30s old: waited out, then asked.
    assert_eq!(
        login_through(Some(NOW - 30.0), &[], fetched(5.0)),
        (ok_at(31.0), vec![31.0])
    );
    // Another session writes a copy 20s into that wait. Asked once the first
    // copy is out, Claude Code would answer from the new one (as the check
    // did before), so that one is waited out too.
    let e = read_fresh(
        &mut || Ok(Some(NOW + 20.0)),
        &mut || NOW + 31.0,
        &mut |_| Ok(fetched(5.0)),
    )
    .unwrap_err();
    assert!(
        matches!(&e, Unread::Copy(e) if e.contains("snapshot 11s")),
        "{e:?}"
    );
    assert_eq!(
        login_through(Some(NOW - 30.0), &[20.0], fetched(5.0)),
        (ok_at(81.0), vec![81.0])
    );
    // Or writes one while it's being asked: the answer may be that copy.
    assert_eq!(
        login_through(Some(NOW - 30.0), &[32.0], fetched(5.0)),
        (ok_at(93.0), vec![31.0, 93.0])
    );
    // A login that fails says so, even when a copy raced its first asking.
    let none =
        json!({"subscription_type": null, "rate_limits_available": false, "rate_limits": null});
    let (e, asked) = login_through(None, &[1.0], none);
    assert!(matches!(e, Err(Unshown::LoginFailed(e)) if e.contains("logged in")));
    assert_eq!(asked, [0.0, 62.0]);
    // A rate-limited endpoint answering from a copy that changed while it
    // was asked shows the login working, as on its own (login_shown): not
    // waited out again, and returns no live reading to record.
    assert_eq!(
        login_logged(
            Some(NOW - 30.0),
            &[32.0],
            from_the_copy(10.0),
            &refused_with(429)
        ),
        (Ok(None), vec![31.0])
    );
    let (e, asked) = login_logged(None, &[1.0], from_the_copy(10.0), &refused_with(401));
    assert!(e.is_err());
    assert_eq!(asked, [0.0, 62.0]);
    // Copies written faster than they can be waited out: the login isn't
    // checked, and it says so rather than call it failed.
    let often: Vec<f64> = (0..10).map(|n| 20.0 + 30.0 * f64::from(n)).collect();
    let (e, asked) = login_through(Some(NOW - 30.0), &often, fetched(5.0));
    let Err(Unshown::Unknown(e)) = e else {
        panic!("{e:?}")
    };
    assert!(e.contains("login wasn't checked"), "{e}");
    assert_eq!(asked, [] as [f64; 0]);
    // A copy dated ahead of the clock isn't waited out: the reading refuses it.
    let (e, asked) = login_through(Some(NOW + 5.0), &[], fetched(5.0));
    assert!(matches!(e, Err(Unshown::Unknown(e)) if e.contains("snapshot")));
    assert_eq!(asked, [0.0]);
}

#[test]
fn only_a_config_showing_no_copy_counts_as_none() {
    assert_eq!(copied_in(&json!({})), Ok(None));
    assert_eq!(
        copied_in(&json!({"cachedUsageUtilization": {"fetchedAtMs": 5000}})),
        Ok(Some(5.0))
    );
    assert!(parsed(b"{\"cachedUsage").is_err());
    assert!(copied_in(&json!({"cachedUsageUtilization": {}})).is_err());
}

/// Claude Code's config as 2.1.330 keeps it, logged in to UUID, with a
/// copy of its endpoint's answer for OWNER fetched at AT.
fn config_with(uuid: &str, owner: &str, at: f64, used: f64) -> Value {
    json!({
        "oauthAccount": {"accountUuid": uuid},
        "cachedUsageUtilization": {
            "fetchedAtMs": at * 1000.0,
            "accountUuid": owner,
            "utilization": fetched(used)["rate_limits"].clone(),
        }
    })
}

#[test]
fn the_copy_is_a_reading_from_when_it_was_fetched() {
    let config = config_with("a", "a", NOW - 300.0, 57.0);
    let (at, copy) = copy_in(&config).unwrap().unwrap();
    assert_eq!(at, NOW - 300.0);
    assert_eq!(Ok(copy), usage(&fetched(57.0), None, NOW));
    // Another account's, as after a login as someone else, is none.
    assert_eq!(copy_in(&config_with("a", "b", NOW, 57.0)), Ok(None));
    let mut logged_out = config_with("a", "a", NOW, 57.0);
    logged_out.as_object_mut().unwrap().remove("oauthAccount");
    assert_eq!(copy_in(&logged_out), Ok(None));
    assert_eq!(copy_in(&json!({})), Ok(None));
    // One it can't make sense of is an error, not a reading.
    let mut broken = config_with("a", "a", NOW, 57.0);
    broken["cachedUsageUtilization"]["utilization"]["seven_day"] = Value::Null;
    assert!(copy_in(&broken).is_err());
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
    fn copy(&mut self) -> Option<(f64, Reading)> {
        None
    }
    fn reset(&mut self, keep: &mut dyn FnMut() -> Result<(), String>) -> Reset {
        self.pressed += 1;
        keep().unwrap();
        Reset::Pressed("Weekly limit refilled".to_owned())
    }
}

fn pass_over(answers: Vec<(Value, Option<f64>)>, state: State) -> (Outcome, usize) {
    let mut answers = Answers {
        answers,
        pressed: 0,
    };
    let o = pass(&mut answers, &mut |_| Ok(()), state, 2.0, NOW, "now");
    (o, answers.pressed)
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

#[test]
fn ask_usage_lets_claude_exit_cleanly_before_killing() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("finished");
    let script = dir.path().join("claude");
    burrow_test::executable(
        &script,
        format!(
            "#!/bin/sh\nread -r _req\nprintf '%s\\n' '{{\"type\":\"control_response\",\"response\":{{\"subtype\":\"success\",\"request_id\":\"usage\",\"response\":{{\"rate_limits_available\":true,\"rate_limits\":{{\"seven_day\":{{\"utilization\":1.0,\"resets_at\":\"2030-01-01T00:00:00Z\"}},\"five_hour\":null,\"limits\":[]}}}}}}}}'\nsleep 0.2\n: > '{}'\n",
            marker.display()
        ),
    );
    let mut answered = false;
    let answer = ask_usage(
        &script,
        dir.path(),
        Duration::from_secs(5),
        None,
        Duration::from_secs(2),
        &mut || {
            answered = true;
            // Sampled on control_response, before the child finishes its shutdown write.
            assert!(!marker.exists(), "on_answer ran after child exited");
        },
    )
    .unwrap();
    assert!(answered);
    assert_eq!(answer["rate_limits_available"], true);
    assert!(
        marker.exists(),
        "ask_usage killed Claude Code before it could finish exiting cleanly"
    );

    // A hung child that never exits on EOF is still killed after exit_grace.
    let hung = dir.path().join("claude-hung");
    burrow_test::executable(
        &hung,
        "#!/bin/sh\nread -r _req\nprintf '%s\\n' '{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"usage\",\"response\":{\"rate_limits_available\":true}}}'\nsleep 30\n",
    );
    let t0 = Instant::now();
    let answer = ask_usage(
        &hung,
        dir.path(),
        Duration::from_secs(5),
        None,
        Duration::from_millis(150),
        &mut || {},
    )
    .unwrap();
    assert_eq!(answer["rate_limits_available"], true);
    assert!(t0.elapsed() < Duration::from_secs(5));
}

#[test]
fn read_usage_reports_the_endpoints_failure() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("claude");
    burrow_test::executable(
        &script,
        "#!/bin/sh\nlog=/dev/null\nwhile [ $# -gt 0 ]; do\n  case \"$1\" in --debug-file) log=$2; shift ;; esac\n  shift\ndone\nread -r _req\nprintf '%s\\n' '{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"usage\",\"response\":{\"rate_limits_available\":true,\"rate_limits\":{\"seven_day\":{\"utilization\":1.0,\"resets_at\":\"2030-01-01T00:00:00Z\"},\"five_hour\":null}}}}'\necho '[ERROR] Failed to load usage data: Request failed with status code 429' >>\"$log\"\n",
    );
    let err = read_usage(&script, dir.path()).unwrap_err();
    assert!(
        err.contains("status code 429"),
        "read_usage dropped the endpoint failure: {err}"
    );
}
