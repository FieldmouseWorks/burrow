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

#[test]
fn a_login_hold_is_invisible_to_the_previous_binary() {
    // An earlier proj reads only the usage record, so it neither holds for
    // a login nor can its refusal updates, meter releases, reminders or
    // settlement alter one: rolled back, there is simply no login hold.
    let (d, cfg) = rig();
    let t = now();
    login(&cfg, "web-1", SAID, t).unwrap();
    assert!(!cfg.jobs.join(FILE).exists());
    let text = fs::read_to_string(cfg.jobs.join(LOGIN_FILE)).unwrap();
    assert!(!text.contains("\"login\""), "{text}");
    assert!(kept(&cfg).unwrap().login);
    // The earlier binary's usage refusal lands in the usage record, later
    // than the login's: the usage hold wins, as the later refusal.
    let old = Spent {
        owner: false,
        until: t + 7200,
        said: "weekly limit".into(),
        job: "web-2".into(),
        at: t + 1,
        back: None,
        admission: None,
        told: t + 1,
        last: Some(t + 1),
        login: false,
    };
    fs::write(cfg.jobs.join(FILE), serde_json::to_string(&old).unwrap()).unwrap();
    let s = kept(&cfg).unwrap();
    assert!(!s.login && s.job == "web-2", "{s:?}");
    // A usage refusal by this binary replaces the login hold outright and
    // is told at once.
    let (d2, cfg2) = rig();
    login(&cfg2, "web-1", SAID, t).unwrap();
    let s = record(&cfg2, "web-2", Some(t + 7200), "weekly limit", t + 1).unwrap();
    assert!(!s.login && s.until == t + 7200);
    assert!(!cfg2.jobs.join(LOGIN_FILE).exists());
    assert_eq!(opens(&d2).len(), 1);
    // And a login hold replaces a settled usage record, so the old record
    // can't be released, reminded or settled under it.
    let (_d, cfg3) = rig();
    record(&cfg3, "web-2", Some(t + 7200), "weekly limit", t).unwrap();
    back(&cfg3, Some(t + 1));
    login(&cfg3, "web-3", SAID, t + 2).unwrap();
    assert!(!cfg3.jobs.join(FILE).exists());
    assert!(out(&cfg3).is_some_and(|s| s.login));
    drop(d);
}

#[test]
fn a_meter_releases_only_a_usage_hold_judged_under_the_lock() {
    // A login refusal that replaced the usage hold meanwhile is not
    // released by a meter evaluated against the usage hold before it.
    let (_d, cfg) = rig();
    let t = now();
    record(&cfg, "web-1", Some(t + 7200), "weekly limit", t).unwrap();
    assert!(!kept(&cfg).unwrap().login);
    let mut s = kept(&cfg).unwrap();
    s.login = true;
    keep(&cfg, &s).unwrap();
    release(&cfg, Some(t + 100), true);
    assert!(out(&cfg).is_some_and(|s| s.login));
    // The same read releases an ordinary usage hold.
    s.login = false;
    keep(&cfg, &s).unwrap();
    release(&cfg, Some(t + 100), true);
    assert_eq!(out(&cfg), None);
}

#[test]
fn a_changed_cause_keeps_the_latest_refusal() {
    // A usage refusal at T arriving after a login refusal at T+1 doesn't
    // move the watermark back, so a run started at T+1 can't release it.
    let (_d, cfg) = rig();
    let t = now();
    login(&cfg, "web-1", SAID, t + 1).unwrap();
    let s = record(&cfg, "web-2", Some(t + 7200), "weekly limit", t).unwrap();
    assert!(!s.login);
    assert_eq!(s.refused(), t + 1);
    back(&cfg, Some(t + 1));
    assert!(out(&cfg).is_some());
    // And the other way round.
    let (_d, cfg) = rig();
    record(&cfg, "web-2", Some(t + 7200), "weekly limit", t + 1).unwrap();
    let s = login(&cfg, "web-1", SAID, t).unwrap();
    assert_eq!(s.refused(), t + 1);
}
