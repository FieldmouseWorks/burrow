// The machine's state from a run Claude Code couldn't authenticate, as on
// 2026-11-16: held like a spent plan, and the owner told to log in.

use crate::engine::claude::LOGGED_OUT;
use crate::usage::tests::{rig, told};
use crate::usage::*;

const LIMIT: &str = "You've hit your monthly spend limit";

#[test]
fn an_expired_login_holds_the_machine_and_tells_the_owner_to_log_in() {
    let (d, cfg) = rig();
    let t = now();
    // No reset to wait for: a try at a time.
    let s = record(&cfg, "web-1", None, LOGGED_OUT, t).unwrap();
    assert_eq!(
        (s.until, s.said.as_str(), s.job.as_str()),
        (t + UNSAID, LOGGED_OUT, "web-1")
    );
    assert_eq!(out(&cfg), Some(s.clone()));
    assert_eq!(
        told(&d),
        ["open proj-usage Claude Code's login expired: log in again on this machine"]
    );
    for words in [line(&s), body(&cfg, &s)] {
        assert!(words.contains("/login"), "{words}");
        assert!(!words.contains("out of usage"), "{words}");
        // Nothing of spend caps: logging in is the owner's to do.
        assert!(!words.contains(policy(&cfg)), "{words}");
    }
    // Every other running turn is refused too: told once.
    record(&cfg, "web-2", None, LOGGED_OUT, t + 5).unwrap();
    assert_eq!(told(&d).len(), 1);
    // The first run that started after the last refusal and answers ends it.
    back(&cfg, Some(t + 5));
    assert!(out(&cfg).is_some());
    back(&cfg, Some(t + 6));
    assert_eq!(out(&cfg), None);
    assert_eq!(
        told(&d)[1..],
        ["close proj-usage Claude Code is logged in again, and proj's jobs are running."]
    );
}

/// What the refused turn is told when it runs again says what it waited
/// for, and a spent plan's still says its own.
#[test]
fn the_refused_turn_is_told_its_login_was_gone() {
    let (_d, cfg) = rig();
    let login = resumed(&cfg, LOGGED_OUT);
    assert!(
        login.contains("login on this machine had expired"),
        "{login}"
    );
    assert!(!login.contains("out of usage") && !login.contains(policy(&cfg)));
    let limit = resumed(&cfg, "Claude reached its weekly allowance");
    assert_eq!(
        limit,
        format!(
            "{}\n\nClaude reached its weekly allowance. {}",
            crate::turn::SPENT,
            policy(&cfg)
        )
    );
}

/// A hold says what its reset is from: a login gone once a limit's hold
/// lapsed is told as a login, and a limit met while logged out, resetting
/// later, as that limit.
#[test]
fn the_hold_is_told_as_the_refusal_its_reset_is_from() {
    let (d, cfg) = rig();
    let t = now();
    record(&cfg, "web-1", Some(t + 7200), LIMIT, t).unwrap();
    let mut lapsed = kept(&cfg).unwrap();
    lapsed.until = t - 1;
    keep(&cfg, &lapsed).unwrap();
    let s = record(&cfg, "web-2", None, LOGGED_OUT, t).unwrap();
    assert_eq!((s.until, s.said.as_str()), (t + UNSAID, LOGGED_OUT));
    assert!(told(&d)[1].starts_with("open proj-usage Claude Code's login expired"));
    // One refused sooner changes nothing of it.
    let s = record(&cfg, "web-3", Some(t + 60), LIMIT, t + 1).unwrap();
    assert_eq!((s.until, s.said.as_str()), (t + UNSAID, LOGGED_OUT));
    let s = record(&cfg, "web-4", Some(t + 2 * UNSAID), LIMIT, t + 2).unwrap();
    assert_eq!(
        (s.until, s.said.as_str()),
        (t + 2 * UNSAID, "Claude reached its extra-usage cap")
    );
    assert_eq!(told(&d).len(), 2);
}
