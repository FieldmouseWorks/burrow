// A run refused for Claude Code's login (2026-11-12, 17:50 to 19:11: its
// OAuth session couldn't be refreshed): held a few minutes at a time, each
// next run testing it, and the owner told once it has lasted the deadline.

use super::*;

const SAID: &str = "Failed to authenticate: OAuth session expired and could not be refreshed";

fn opens(d: &tempfile::TempDir) -> Vec<String> {
    told(d)
        .into_iter()
        .filter(|l| l.starts_with("open "))
        .collect()
}

/// The record as if it were first refused `ago` seconds since, and its hold
/// `lapsed` or not.
fn aged(cfg: &Config, ago: i64, lapsed: bool) {
    let mut s = kept(cfg).unwrap();
    s.at = now() - ago;
    s.last = Some(s.at);
    if lapsed {
        s.until = now() - 1;
    }
    keep(cfg, &s).unwrap();
}

#[test]
fn a_refused_login_holds_a_while_and_is_tested_again_telling_no_one() {
    let (d, cfg) = rig();
    let t = now();
    let s = login(&cfg, "web-1", SAID, t).unwrap();
    assert!(s.login);
    assert_eq!(
        (s.until, s.at, s.job.as_str(), s.said.as_str()),
        (t + login::RETRY, t, "web-1", SAID)
    );
    assert_eq!(out(&cfg), Some(s.clone()));
    assert!(
        line(&s).starts_with("Claude Code's login was refused"),
        "{}",
        line(&s)
    );
    // A login that comes back by itself needs no one.
    remind(&cfg);
    tend(&cfg);
    assert!(told(&d).is_empty(), "{:?}", told(&d));
    // A meter read says nothing of the login.
    metered(&cfg);
    assert!(out(&cfg).is_some());
    // The next run after the hold, refused again: held again, counted from
    // the first refusal.
    aged(&cfg, 10, true);
    assert_eq!(out(&cfg), None);
    let again = login(&cfg, "web-2", SAID, now()).unwrap();
    assert_eq!(again.job.as_str(), "web-1");
    assert_eq!(again.at, now() - 10);
    assert!(again.holds(now()));
    assert!(told(&d).is_empty());
    // A run started since that isn't refused ends it, and the owner was
    // never told (the close of a notice never opened asks GitHub nothing).
    back(&cfg, Some(now() + 1));
    assert_eq!(out(&cfg), None);
    assert!(opens(&d).is_empty(), "{:?}", told(&d));
}

#[test]
fn a_login_refused_past_its_deadline_raises_one_notice() {
    // Refused again once the hold lapsed, past the deadline.
    let (d, cfg) = rig();
    login(&cfg, "web-1", SAID, now()).unwrap();
    aged(&cfg, login::DEADLINE, true);
    let s = login(&cfg, "web-2", SAID, now()).unwrap();
    assert!(s.holds(now()));
    let open = opens(&d);
    assert_eq!(open.len(), 1, "{open:?}");
    assert!(
        open[0].starts_with("open proj-usage Claude Code needs signing in again"),
        "{open:?}"
    );
    assert!(body(&cfg, &s).contains(SAID));
    // Every other run refused, and every look meanwhile: still the one.
    login(&cfg, "web-3", SAID, now()).unwrap();
    remind(&cfg);
    tend(&cfg);
    assert_eq!(opens(&d).len(), 1);
    // Back: the notice it raised is closed.
    back(&cfg, Some(now() + 1));
    assert_eq!(out(&cfg), None);
    let last = told(&d).pop().unwrap();
    assert!(
        last.starts_with("close proj-usage Claude Code's login works again"),
        "{last}"
    );

    // Or past its deadline while it still holds: the next look tells.
    let (d, cfg) = rig();
    login(&cfg, "web-1", SAID, now()).unwrap();
    aged(&cfg, login::DEADLINE, false);
    assert!(out(&cfg).is_some());
    remind(&cfg);
    remind(&cfg);
    tend(&cfg);
    assert_eq!(opens(&d).len(), 1, "{:?}", told(&d));
}

#[test]
fn a_usage_limit_and_a_refused_login_each_keep_their_own_hold() {
    // A usage limit while a login holds replaces it, and is told at once.
    let (d, cfg) = rig();
    let t = now();
    login(&cfg, "web-1", SAID, t).unwrap();
    let s = record(
        &cfg,
        "web-2",
        Some(t + 7200),
        "You've hit your weekly limit",
        t,
    )
    .unwrap();
    assert!(!s.login);
    assert_eq!((s.until, s.job.as_str()), (t + 7200, "web-2"));
    assert_eq!(opens(&d).len(), 1);
    assert!(opens(&d)[0].starts_with("open proj-usage Claude is out of usage"));
    // A login refused while a usage limit holds moves it on, no more.
    let s = login(&cfg, "web-3", SAID, t + 1).unwrap();
    assert!(!s.login);
    assert_eq!((s.until, s.job.as_str()), (t + 7200, "web-2"));
    assert_eq!(opens(&d).len(), 1);

    // A short login hold doesn't cut a usage limit short.
    let (_d, cfg) = rig();
    login(&cfg, "web-1", SAID, t).unwrap();
    let s = record(&cfg, "web-2", None, "Claude AI usage limit reached", t).unwrap();
    assert_eq!(s.until, t + UNSAID);
}
