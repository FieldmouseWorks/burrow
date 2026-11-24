// The machine's state from a run Claude Code couldn't authenticate, as on
// 2026-11-16: held like a spent plan, and the owner told to log in.

use super::LOGGED_OUT;
use crate::usage::login::{holds_machine, moved_to, resumed};
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
        ["open proj-usage-claude Claude Code's login expired: log in again on this machine"]
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
        ["close proj-usage-claude Claude Code is logged in again, and proj's jobs are running."]
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

/// A refused run whose route moved its turn to another engine is told the
/// move and why, not that usage is back, nor to wait for it.
#[test]
fn the_moved_turn_is_told_where_it_went() {
    let (_d, cfg) = rig();
    let said = moved_to("claude-api", true, "Claude reached its weekly allowance");
    let told = resumed(&cfg, &said);
    assert!(
        told.starts_with("Your last run was refused before it finished (Claude reached its weekly allowance), so this turn carries on on claude-api, "),
        "{told}"
    );
    assert!(
        !told.contains("It's back now") && !told.contains(policy(&cfg)),
        "{told}"
    );
    let back = resumed(
        &cfg,
        &moved_to(
            "claude",
            true,
            "claude-api refused: its credit has no room: today's allowance spent",
        ),
    );
    assert!(back.contains("(claude-api refused: its credit has no room: today's allowance spent), so this turn carries on on claude, "), "{back}");
    let cross = resumed(
        &cfg,
        &moved_to("engine-b", false, "claude reached its allowance"),
    );
    assert!(
        cross.contains(
            "(claude reached its allowance), so this turn carries on on engine-b in a fresh conversation."
        ),
        "{cross}"
    );
}

/// A hold says what its reset is from: a login gone once a limit's hold
/// lapsed is told as a login, and a limit met while logged out, resetting
/// later, as that limit, the owner's notice told again at once.
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
    assert!(told(&d)[1].starts_with("open proj-usage-claude Claude Code's login expired"));
    // One refused sooner changes nothing of it.
    let s = record(&cfg, "web-3", Some(t + 60), LIMIT, t + 1).unwrap();
    assert_eq!((s.until, s.said.as_str()), (t + UNSAID, LOGGED_OUT));
    let s = record(&cfg, "web-4", Some(t + 2 * UNSAID), LIMIT, t + 2).unwrap();
    assert_eq!(
        (s.until, s.said.as_str()),
        (t + 2 * UNSAID, "Claude reached its extra-usage cap")
    );
    assert_eq!(told(&d).len(), 3);
    assert!(told(&d)[2].starts_with("open proj-usage-claude Claude is out of usage until"));
}

/// While a hold stands, the owner's notice follows what it's for at once,
/// each way, not at the reminder: a short limit's hold that a login gone
/// outlasts tells them to log in, even refused in the same second, and a
/// limit outlasting that tells them of the limit. The same cause again
/// tells nothing new.
#[test]
fn a_standing_holds_notice_follows_its_cause_each_way() {
    let (d, cfg) = rig();
    let t = now();
    record(&cfg, "web-1", Some(t + 60), LIMIT, t).unwrap();
    assert!(told(&d)[0].starts_with("open proj-usage-claude Claude is out of usage until"));
    // Refused for the login in the same second: the hold is the login's now.
    let s = record(&cfg, "web-2", None, LOGGED_OUT, t).unwrap();
    assert_eq!((s.until, s.said.as_str()), (t + UNSAID, LOGGED_OUT));
    assert_eq!(
        told(&d)[1..],
        ["open proj-usage-claude Claude Code's login expired: log in again on this machine"]
    );
    assert!(body(&cfg, &s).contains("/login"));
    // The same cause again: deduplicated, as before.
    record(&cfg, "web-3", None, LOGGED_OUT, t + 5).unwrap();
    assert_eq!(told(&d).len(), 2);
    // A limit that outlasts it: told as the limit again, at once.
    let s = record(&cfg, "web-4", Some(t + 2 * UNSAID), LIMIT, t + 6).unwrap();
    assert_eq!(s.until, t + 2 * UNSAID);
    assert_eq!(told(&d).len(), 3);
    assert!(told(&d)[2].starts_with("open proj-usage-claude Claude is out of usage until"));
    // And another limit refusal changes nothing told.
    record(&cfg, "web-5", Some(t + 2 * UNSAID + 60), LIMIT, t + 7).unwrap();
    assert_eq!(told(&d).len(), 3);
}

/// A sandbox's login is its own token, not this machine's: one expired
/// there holds no turn here, while this machine's login still holds the
/// turns here, and a limit met in a sandbox (the plan is the same) every
/// turn.
#[test]
fn only_this_machines_login_holds_the_machine() {
    assert!(!holds_machine(LOGGED_OUT, true));
    assert!(holds_machine(LOGGED_OUT, false));
    assert!(holds_machine(LIMIT, true));
    assert!(holds_machine(LIMIT, false));
    assert!(login::SANDBOX.contains("engines.claude.credentialFile"));
    assert!(!login::SANDBOX.contains("/login"));
}

/// This machine's login is a credential a sandbox doesn't use: its hold
/// holds no sandbox's turn, while a limit, which every token's plan shares,
/// holds them all.
#[test]
fn this_machines_login_hold_lets_a_sandboxs_turn_by() {
    use crate::usage::login::{Run, out_for};
    let (_d, cfg) = rig();
    let t = now();
    let s = record(&cfg, "web-1", None, LOGGED_OUT, t).unwrap();
    assert_eq!(out_for(&cfg, &Run::plan(false)), Some(s.clone()));
    assert_eq!(out_for(&cfg, &Run::plan(true)), None);
    // The owner's pause holds every turn, whatever it says.
    assert!(
        Spent {
            owner: true,
            ..s.clone()
        }
        .holds_run(&Run::plan(true))
    );
    // A limit met meanwhile is what holds now: the sandbox's turn too.
    record(&cfg, "web-2", Some(t + 2 * UNSAID), LIMIT, t + 1).unwrap();
    assert!(out_for(&cfg, &Run::plan(true)).is_some());
}

/// A sandbox's answer proves its own token, not this machine's login: it
/// ends no login hold here, and ends a limit's, the plan's.
#[test]
fn a_sandboxs_answer_ends_a_limit_but_no_login_hold_here() {
    let (d, cfg) = rig();
    let t = now();
    record(&cfg, "web-1", None, LOGGED_OUT, t).unwrap();
    answered(&cfg, t + 6, true);
    assert!(out(&cfg).is_some());
    assert_eq!(told(&d).len(), 1, "nothing closed");
    answered(&cfg, t + 6, false);
    assert_eq!(out(&cfg), None);

    let (_d, cfg) = rig();
    record(&cfg, "web-1", Some(t + 3600), LIMIT, t).unwrap();
    answered(&cfg, t + 6, true);
    assert_eq!(out(&cfg), None);
}

/// The credit's turns run on neither the plan nor this machine's login: no
/// hold of theirs holds a `claude-api` run, which a turn's protected
/// identity says it is; the owner's pause holds it like any other.
#[test]
fn no_plan_hold_holds_an_api_run() {
    use crate::engine::Choice;
    use crate::usage::login::{Run, out_for, run_of};
    let (_d, cfg) = rig();
    let t = now();
    let api = Run::new("claude-api", false);
    let s = record(&cfg, "web-1", None, LOGGED_OUT, t).unwrap();
    assert_eq!(out_for(&cfg, &api), None);
    record(&cfg, "web-2", Some(t + 2 * UNSAID), LIMIT, t + 1).unwrap();
    assert_eq!(out_for(&cfg, &api), None);
    assert!(out_for(&cfg, &Run::plan(false)).is_some());
    assert!(
        Spent {
            engine: ALL_ENGINES.into(),
            owner: true,
            ..s
        }
        .holds_run(&api)
    );
    let job = crate::job::Job::new(&cfg, "api-1");
    std::fs::create_dir_all(&job.dir).unwrap();
    job.set_turns(1).unwrap();
    assert_eq!(run_of(&cfg, "api-1"), Run::plan(false));
    let choice = Choice {
        engine: "claude-api".into(),
        ..Choice::default()
    };
    crate::retry::Identity::capture_choice(&job, "1", &choice).unwrap();
    assert_eq!(run_of(&cfg, "api-1"), api);
}
