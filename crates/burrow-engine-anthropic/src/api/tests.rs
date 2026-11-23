//! Unit tests for `claude-api` credentials, credit meter admission, and API
//! refusal classification.

use std::fs;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use burrow_settings::credit::ENGINE;
use serde_json::json;

use super::*;
use crate::claude;

const KEY: &str = "sk-ant-stand-in-key";
const M: i64 = 1_000_000;

struct Rig {
    tmp: tempfile::TempDir,
    jobs: PathBuf,
    credit_file: PathBuf,
    key_file: Option<PathBuf>,
    now: i64,
}

/// A machine with a $20 credit expiring in two days with $5 in reserve, so
/// $10 a day may go, and a key; jobs under it.
fn rig() -> Rig {
    let tmp = tempfile::tempdir().unwrap();
    let jobs = tmp.path().join("jobs");
    fs::create_dir_all(&jobs).unwrap();
    let ts = jiff::Timestamp::now();
    let now = ts.as_second();
    let renews = ts
        .to_zoned_datetime(jiff::tz::TimeZone::UTC)
        .date()
        .checked_add(jiff::Span::new().days(2))
        .unwrap();
    let credit_file = tmp.path().join("overlay.credit.json");
    fs::write(
        &credit_file,
        json!({
            "credit": {
                "budgetUsd": 20,
                "renews": renews.to_string(),
                "monthlyUsd": 200,
                "reserveUsd": 5,
                "since": now,
            }
        })
        .to_string(),
    )
    .unwrap();
    let key_file = tmp.path().join("api-key");
    fs::write(&key_file, format!("{KEY}\n")).unwrap();
    Rig {
        tmp,
        jobs,
        credit_file,
        key_file: Some(key_file),
        now,
    }
}

impl Rig {
    /// Turn N of JOB on `claude-api`, started now, with STREAM.
    fn turn(&self, job: &str, n: u64, stream: &[serde_json::Value]) {
        let dir = self.jobs.join(job);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("{n}.engine")), ENGINE).unwrap();
        fs::write(dir.join(format!("{n}.started")), self.now.to_string()).unwrap();
        let text: String = stream.iter().map(|r| format!("{r}\n")).collect();
        fs::write(dir.join(format!("{n}.jsonl")), text).unwrap();
    }
}

#[test]
fn short_credit_and_bad_keys_are_refused() {
    let r = rig();
    assert!(room(Some(&r.credit_file), &r.jobs, r.now));
    assert!(!short(Some(&r.credit_file), &r.jobs, 50.0, r.now));

    // Missing, unreadable, or empty keys.
    assert!(
        key(None)
            .unwrap_err()
            .to_string()
            .contains("no API key is configured")
    );
    let absent = r.tmp.path().join("absent");
    assert!(
        key(Some(&absent))
            .unwrap_err()
            .to_string()
            .contains("can't be read")
    );
    let empty = r.tmp.path().join("empty-key");
    fs::write(&empty, "\n").unwrap();
    assert!(
        key(Some(&empty))
            .unwrap_err()
            .to_string()
            .contains("is empty")
    );

    // Unconfigured settings, malformed credit, credit off, exhausted, pending.
    assert!(
        admit(None, &r.jobs, r.now)
            .unwrap_err()
            .0
            .contains("no settings")
    );
    let bad = r.tmp.path().join("bad.json");
    fs::write(&bad, "{not json").unwrap();
    assert!(
        admit(Some(&bad), &r.jobs, r.now)
            .unwrap_err()
            .0
            .contains("can't be metered")
    );
    let off = r.tmp.path().join("off.json");
    fs::write(&off, r#"{"credit":null}"#).unwrap();
    assert!(
        admit(Some(&off), &r.jobs, r.now)
            .unwrap_err()
            .0
            .contains("no API credit is set")
    );
    r.turn(
        "spent",
        1,
        &[
            json!({"type":"burrow.run","at":r.now,"budget":15 * M}),
            json!({"type":"result","session_id":"a","total_cost_usd":15}),
        ],
    );
    assert!(
        admit(Some(&r.credit_file), &r.jobs, r.now)
            .unwrap_err()
            .0
            .contains("no room: spent down to the reserve")
    );
    assert!(!room(Some(&r.credit_file), &r.jobs, r.now));
    assert!(short(Some(&r.credit_file), &r.jobs, 10.0, r.now));
    assert!(opens(Some(&r.credit_file), &r.jobs, r.now).is_some());
}

/// Admissions take turns: one waits while another holds the lock, so two
/// can't both read the same room before either's marker is written.
#[test]
fn admissions_take_turns_under_one_lock() {
    let r = rig();
    let first = admit(Some(&r.credit_file), &r.jobs, r.now).unwrap();
    assert_eq!(first.budget, 10 * M);
    let (tx, rx) = mpsc::channel();
    let credit_file = r.credit_file.clone();
    let jobs = r.jobs.clone();
    let now = r.now;
    let waiter = std::thread::spawn(move || {
        tx.send(admit(Some(&credit_file), &jobs, now).map(|a| a.budget))
            .unwrap();
    });
    assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
    // Its run started meanwhile, holding today's $10: the waiter gets none.
    r.turn(
        "first",
        1,
        &[json!({"type":"burrow.run","at":r.now,"budget":first.budget})],
    );
    drop(first);
    let second = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    waiter.join().unwrap();
    assert!(second.unwrap_err().0.contains("no room"));
}

/// The key reaches a `claude-api` run on a descriptor alone, with no login
/// beside it; a `claude` run gets no API credential, whatever it inherits.
#[test]
fn each_engine_starts_with_its_own_credentials_only() {
    let r = rig();
    let out = r.tmp.path().join("seen");
    let stand_in = r.tmp.path().join("claude");
    burrow_test::executable(
        &stand_in,
        format!(
            "#!/bin/sh\nenv | cut -d= -f1 | grep -E '^(ANTHROPIC|CLAUDE_CODE)_' | sort >'{0}.names'\n\
             if [ -n \"$CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR\" ]; then cat \"/dev/fd/$CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR\" >'{0}.key'; fi\n",
            out.display()
        ),
    );
    let inherited = [
        ("ANTHROPIC_API_KEY", "inherited-key"),
        ("ANTHROPIC_AUTH_TOKEN", "inherited-token"),
        ("CLAUDE_CODE_OAUTH_TOKEN", "the-login"),
        ("CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR", "9"),
        ("ANTHROPIC_BASE_URL", "http://127.0.0.1:1"),
    ];
    let run = |mode| {
        let _ = fs::remove_file(out.with_extension("key"));
        let mut c = Command::new(&stand_in);
        // What it inherits is only what's here: not the test's own session.
        c.env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .envs(inherited);
        let key = credentials(&mut c, mode, r.key_file.as_deref()).unwrap();
        assert!(c.status().unwrap().success());
        drop(key);
        (
            fs::read_to_string(out.with_extension("names")).unwrap(),
            fs::read_to_string(out.with_extension("key")).ok(),
        )
    };
    let (names, key) = run(Mode::Claude);
    assert_eq!(names, "ANTHROPIC_BASE_URL\nCLAUDE_CODE_OAUTH_TOKEN\n");
    assert_eq!(key, None);
    let (names, key) = run(Mode::ClaudeApi);
    assert_eq!(
        names,
        "ANTHROPIC_BASE_URL\nCLAUDE_CODE_API_KEY_FILE_DESCRIPTOR\n"
    );
    assert_eq!(key.as_deref(), Some(format!("{KEY}\n").as_str()));
    // No key, no start: the refusal names the file, never a key.
    let mut c = Command::new(&stand_in);
    assert!(credentials(&mut c, Mode::ClaudeApi, None).is_err());
}

/// A stream ending as Claude Code ends a run on a key the API won't take.
fn rejected_key() -> Vec<serde_json::Value> {
    let said = "Invalid API key · Fix external API key";
    vec![
        json!({"type":"system","subtype":"init","session_id":"s","apiKeySource":"ANTHROPIC_API_KEY"}),
        json!({"type":"assistant","error":"authentication_failed","session_id":"s","message":{"content":[{"type":"text","text":said}]}}),
        json!({"type":"result","subtype":"success","is_error":true,"result":said,"session_id":"s","total_cost_usd":0}),
    ]
}

#[test]
fn the_apis_refusals_are_the_credits_own_and_hold_no_plan() {
    let key = rejected_key();
    let last = key.last().unwrap();
    assert_eq!(
        refusal(last, key.clone().into_iter()).unwrap().0,
        "the API didn't take its key"
    );
    // The plan's classifier calls the same stream a lost login, which holds
    // every turn; the API engine's runs never reach it.
    let tmp = tempfile::tempdir().unwrap();
    let stream = tmp.path().join("1.jsonl");
    let text: String = key.iter().map(|r| format!("{r}\n")).collect();
    fs::write(&stream, text).unwrap();
    assert!(claude::refused_file(last, &stream, 0, 0).is_some());
    assert!(refusal_file(last, &stream, 0).is_some());
    // A key it couldn't read: Claude Code reads no login instead, and says so.
    let unread = vec![
        json!({"type":"system","subtype":"init","session_id":"s","apiKeySource":"none"}),
        json!({"type":"result","is_error":true,"result":"Not logged in · Please run /login"}),
    ];
    assert_eq!(
        refusal(unread.last().unwrap(), unread.clone().into_iter())
            .unwrap()
            .0,
        "the API didn't take its key"
    );
    let other_source = json!({"type":"result","is_error":true,"result":"Execution error"});
    assert!(refusal(&other_source, unread.into_iter()).is_some());
    let budget = json!({"type":"result","subtype":"error_max_budget_usd","is_error":true,"total_cost_usd":10});
    assert!(
        refusal(&budget, std::iter::empty())
            .unwrap()
            .0
            .contains("budget")
    );
    for said in [
        "Your credit balance is too low to access the Anthropic API.",
        "You have reached your specified workspace API usage limits.",
    ] {
        let last = json!({"type":"result","is_error":true,"result":said});
        assert!(
            refusal(&last, std::iter::empty())
                .unwrap()
                .0
                .contains("credit or spend limit"),
            "{said}"
        );
    }
    // Any other error, and any answer, is the turn's own.
    let other = json!({"type":"result","is_error":true,"result":"Execution error"});
    assert_eq!(refusal(&other, std::iter::empty()), None);
    let fine = json!({"type":"result","is_error":false,"result":"done"});
    assert_eq!(refusal(&fine, rejected_key().into_iter()), None);
}

#[test]
fn a_budget_is_given_to_the_micro_dollar() {
    assert_eq!(usd(10 * M), "10.000000");
    assert_eq!(usd(1_234_567), "1.234567");
    assert_eq!(usd(5), "0.000005");
}

impl Rig {
    /// The credit's meter as admission reads it.
    fn meter(&self) -> burrow_settings::credit::Meter {
        burrow_probe::credit::meter(&self.credit_file, Some(&self.jobs), self.now).unwrap()
    }

    fn reserve(&self, cap: Option<i64>) -> Reservation {
        reserve(Some(&self.credit_file), &self.jobs, self.now, cap).unwrap()
    }
}

/// What a run's stdout says of SESSION and its cumulative COST, with an
/// assistant message the ledger doesn't need and a line cut short.
fn ran(session: &str, cost: f64) -> Vec<u8> {
    format!(
        "{}\n{}\n{}\n{{\"type\":\"res",
        json!({"type":"system","subtype":"init","session_id":session}),
        json!({"type":"assistant","session_id":session,"message":{"id":"m","content":[]}}),
        json!({"type":"result","session_id":session,"total_cost_usd":cost,"usage":{"input_tokens":1}}),
    )
    .into_bytes()
}

/// A run admitted outside proj's jobs holds its budget in the meter's ledger
/// from its admission until its cost is recorded, so the next admission
/// gets only what's left: before, every run got the same room again.
#[test]
fn a_reserved_run_is_held_at_its_budget_then_charged_its_cost() {
    let r = rig();
    let first = r.reserve(None);
    assert_eq!(first.budget, 10 * M);
    let m = r.meter();
    assert_eq!((m.spent, m.headroom()), (10 * M, 0), "held at its budget");
    // Another admission while it runs finds no room, and doesn't wait.
    let refused = reserve(Some(&r.credit_file), &r.jobs, r.now, None).unwrap_err();
    assert!(refused.0.contains("no room"), "{refused}");

    first.finish(&ran("s1", 1.5)).unwrap();
    assert_eq!(r.meter().spent, 1_500_000, "charged its cost alone");
    let second = r.reserve(None);
    assert_eq!(second.budget, 10 * M - 1_500_000);
    // Its stream keeps the marker and what says its session and cost.
    let kinds: Vec<String> = fs::read_to_string(r.jobs.join(".engine-runs/1.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap()["type"].to_string())
        .collect();
    assert_eq!(kinds, [r#""burrow.run""#, r#""system""#, r#""result""#]);
}

/// A run's budget is the least of the credit's room and its own cap.
#[test]
fn a_reservation_takes_its_cap_when_less() {
    let r = rig();
    assert_eq!(r.reserve(Some(M)).budget, M);
    assert_eq!(r.meter().spent, M);
    assert_eq!(r.reserve(Some(100 * M)).budget, 9 * M);
}

/// A run that never started gives its reservation back.
#[test]
fn an_abandoned_reservation_holds_nothing() {
    let r = rig();
    r.reserve(None).abandon().unwrap();
    assert_eq!(r.meter().spent, 0);
    assert_eq!(r.reserve(None).budget, 10 * M);
}

/// A run that resumes another's session is charged its own cost, not the
/// session's whole; one stopped before its result stays held at its budget
/// until a later run of its session accounts for it.
#[test]
fn runs_of_one_session_are_each_charged_once() {
    let r = rig();
    r.reserve(None).finish(&ran("s1", 1.0)).unwrap();
    r.reserve(None).finish(&ran("s1", 1.5)).unwrap();
    assert_eq!(r.meter().spent, 1_500_000);

    // Stopped once its session was known, before its result.
    let init = json!({"type":"system","subtype":"init","session_id":"s1"});
    r.reserve(Some(2 * M))
        .finish(format!("{init}\n").as_bytes())
        .unwrap();
    assert_eq!(r.meter().spent, 3_500_000, "held at its budget");
    r.reserve(None).finish(&ran("s1", 2.25)).unwrap();
    assert_eq!(r.meter().spent, 2_250_000, "the resume accounts for it");

    // Stopped before it said its session: nothing can account for it, so
    // it stays held at its budget, the side that never overspends.
    r.reserve(Some(M)).finish(b"").unwrap();
    assert_eq!(r.meter().spent, 3_250_000);
}
