//! The `claude-api` engine against stand-ins: a stand-in engine that says
//! what it was started with, and credit fixtures for every way the credit
//! can be short.
use super::*;
use crate::config::tests::{example, set};
use crate::engine::{Choice, spawn};
use crate::job::Job;
use burrow_settings::credit::ENGINE;
use serde_json::json;
use std::fs;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

const KEY: &str = "sk-ant-stand-in-key";
const M: i64 = 1_000_000;

struct Rig {
    tmp: tempfile::TempDir,
    cfg: Config,
}

/// A machine with a $20 credit expiring in two days with $5 in reserve, so
/// $10 a day may go, and a key; jobs under it.
fn rig() -> Rig {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg: Config = serde_json::from_value(example()).unwrap();
    cfg.jobs = tmp.path().join("jobs");
    cfg.scratch = tmp.path().join("scratch");
    fs::create_dir_all(&cfg.jobs).unwrap();
    let renews = jiff::Timestamp::now()
        .to_zoned_datetime(jiff::tz::TimeZone::UTC)
        .date()
        .checked_add(jiff::Span::new().days(2))
        .unwrap();
    set(
        &mut cfg,
        tmp.path(),
        &[&format!("credit 20 {renews} 200 5")],
    );
    let key = tmp.path().join("api-key");
    fs::write(&key, format!("{KEY}\n")).unwrap();
    cfg.claude_api_key_file = Some(key);
    Rig { tmp, cfg }
}

impl Rig {
    /// Turn N of JOB on `claude-api`, started now, with STREAM.
    fn turn(&self, job: &str, n: u64, stream: &[serde_json::Value]) {
        let dir = self.cfg.jobs.join(job);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("{n}.engine")), ENGINE).unwrap();
        fs::write(dir.join(format!("{n}.started")), crate::now().to_string()).unwrap();
        let text: String = stream.iter().map(|r| format!("{r}\n")).collect();
        fs::write(dir.join(format!("{n}.jsonl")), text).unwrap();
    }

    /// A job about to start turn 1 on `claude-api`, its prompt written, and a
    /// stand-in engine (as timeout(1), where every start goes) that says it
    /// started, what with, and answers `answer`.
    fn launching(&mut self, answer: &str) -> (PathBuf, PathBuf) {
        let prompt = {
            let job = Job::new(&self.cfg, "job");
            fs::create_dir_all(&job.dir).unwrap();
            fs::write(job.turn("1", "engine"), ENGINE).unwrap();
            fs::write(job.path("prompt.1"), "prompt").unwrap();
            fs::write(job.turn("1", "jsonl"), "").unwrap();
            job.path("prompt.1")
        };
        let sentinel = self.tmp.path().join("started");
        let engine = self.tmp.path().join("engine");
        burrow_test::executable(
            &engine,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >>'{}'\nenv >>'{}.env'\nprintf '%s\\n' '{answer}'\n",
                sentinel.display(),
                sentinel.display()
            ),
        );
        self.cfg.tools.timeout = engine;
        (prompt, sentinel)
    }

    fn spawn(&self, prompt: &Path, agent: Option<&str>) -> io::Result<std::process::Child> {
        self.spawn_as(
            prompt,
            agent,
            &Choice {
                engine: Engine::ClaudeApi,
                ..Choice::default()
            },
        )
    }

    fn spawn_as(
        &self,
        prompt: &Path,
        agent: Option<&str>,
        choice: &Choice,
    ) -> io::Result<std::process::Child> {
        let job = Job::new(&self.cfg, "job");
        spawn(
            &self.cfg,
            &job,
            "1",
            prompt,
            &job.path("waits"),
            agent,
            false,
            "",
            choice,
        )
    }
}

fn refused(e: io::Error) -> String {
    Refused::of(&e)
        .unwrap_or_else(|| panic!("not a refusal: {e}"))
        .to_string()
}

#[test]
fn a_launch_is_admitted_with_its_budget_and_starts_as_the_api_engine() {
    let mut r = rig();
    let (prompt, sentinel) =
        r.launching(r#"{"type":"result","session_id":"s","total_cost_usd":1}"#);
    assert!(r.spawn(&prompt, None).unwrap().wait().unwrap().success());
    let argv = fs::read_to_string(&sentinel).unwrap();
    // Its budget: $10 of today's allowance, under the $15 above the reserve.
    assert!(argv.contains("--max-budget-usd 10.000000"), "{argv}");
    assert!(argv.contains(" _claude-api "), "{argv}");
    // No key in its argv or anything it inherits: only exec_claude opens it.
    let env = fs::read_to_string(sentinel.with_extension("env")).unwrap();
    assert!(!argv.contains(KEY) && !env.contains(KEY));
    // The marker holds the budget, which the meter counts until the cost.
    let job = Job::new(&r.cfg, "job");
    let stream = fs::read_to_string(job.turn("1", "jsonl")).unwrap();
    let marker: serde_json::Value = serde_json::from_str(stream.lines().next().unwrap()).unwrap();
    assert_eq!(marker["budget"], 10 * M);
}

/// The settings' model and effort for `claude-api`, and the job's Claude
/// conversation, go with its launch; the turn is recorded as the credit's.
#[test]
fn a_launch_carries_its_tuning_and_the_jobs_conversation() {
    let mut r = rig();
    let (prompt, sentinel) = r.launching(r#"{"type":"result"}"#);
    let job = Job::new(&r.cfg, "job");
    fs::write(job.path("session.claude"), "the-session\n").unwrap();
    let choice = Choice {
        engine: Engine::ClaudeApi,
        model: Some("api-model".into()),
        effort: Some("high".into()),
    };
    job.record_choice("1", &choice).unwrap();
    assert_eq!(fs::read_to_string(job.turn("1", "engine")).unwrap(), ENGINE);
    assert!(
        r.spawn_as(&prompt, None, &choice)
            .unwrap()
            .wait()
            .unwrap()
            .success()
    );
    let argv = fs::read_to_string(&sentinel).unwrap();
    for given in [
        "--model api-model",
        "--effort high",
        "--resume the-session",
        "--max-budget-usd 10.000000",
    ] {
        assert!(argv.contains(given), "{given}: {argv}");
    }
}

/// Every way the credit can be short refuses before the engine starts: no
/// sentinel, no marker, no start recorded.
#[test]
fn a_short_credit_refuses_the_launch_before_the_engine_starts() {
    type Short = fn(&mut Rig);
    let cases: [(&str, Short, &str); 8] = [
        (
            "no key",
            |r| r.cfg.claude_api_key_file = None,
            "no API key is configured",
        ),
        (
            "an unreadable key",
            |r| r.cfg.claude_api_key_file = Some(r.tmp.path().join("absent")),
            "can't be read",
        ),
        (
            "an empty key",
            |r| fs::write(r.cfg.claude_api_key_file.as_ref().unwrap(), "\n").unwrap(),
            "is empty",
        ),
        (
            "an unreadable credit",
            |r| {
                let file = r.cfg.settings.as_ref().unwrap().credit_file();
                fs::write(file, "{not json").unwrap();
            },
            "can't be metered",
        ),
        (
            "no credit set",
            |r| set(&mut r.cfg, Path::new("/"), &["credit off"]),
            "no API credit is set",
        ),
        (
            "an exhausted credit",
            // $15 spent and $5 kept: nothing above the reserve.
            |r| {
                r.turn(
                    "spent",
                    1,
                    &[
                        json!({"type":"burrow.run","at":crate::now(),"budget":15 * M}),
                        json!({"type":"result","session_id":"a","total_cost_usd":15}),
                    ],
                )
            },
            "no room: spent down to the reserve",
        ),
        (
            "a budget committed to a run still going",
            // Admitted moments ago with all of today's $10: no result yet.
            |r| {
                r.turn(
                    "running",
                    1,
                    &[json!({"type":"burrow.run","at":crate::now(),"budget":10 * M})],
                )
            },
            "no room: today's allowance spent",
        ),
        (
            "a run going with no budget recorded",
            |r| r.turn("old", 1, &[json!({"type":"burrow.run","at":crate::now()})]),
            "no recorded cost yet",
        ),
    ];
    for (name, short, says) in cases {
        let mut r = rig();
        let (prompt, sentinel) = r.launching(r#"{"type":"result"}"#);
        short(&mut r);
        let why = refused(r.spawn(&prompt, None).unwrap_err());
        assert!(why.contains(says), "{name}: {why}");
        assert!(why.starts_with("claude-api refused: "), "{name}: {why}");
        assert!(!sentinel.exists(), "{name}: the engine started");
        let job = Job::new(&r.cfg, "job");
        assert_eq!(
            fs::read_to_string(job.turn("1", "jsonl")).unwrap(),
            "",
            "{name}"
        );
        assert!(!job.turn("1", "started").exists(), "{name}");
    }
}

/// A sandbox is given no key: the engine never starts in one.
#[test]
fn a_sandboxed_launch_is_refused() {
    let mut r = rig();
    let (prompt, sentinel) = r.launching(r#"{"type":"result"}"#);
    let why = refused(r.spawn(&prompt, Some("vm")).unwrap_err());
    assert!(why.contains("sandbox"), "{why}");
    assert!(!sentinel.exists());
}

/// A turn's next run (a retry after a kill, or to say what comes next) is
/// admitted again, on what the last one spent: here the last took all of
/// today's allowance, so the retry doesn't start.
#[test]
fn a_retry_is_admitted_again_on_what_the_last_run_spent() {
    let mut r = rig();
    let (prompt, sentinel) =
        r.launching(r#"{"type":"result","session_id":"s","total_cost_usd":10,"is_error":false}"#);
    assert!(r.spawn(&prompt, None).unwrap().wait().unwrap().success());
    let why = refused(r.spawn(&prompt, None).unwrap_err());
    assert!(why.contains("today's allowance spent"), "{why}");
    assert_eq!(fs::read_to_string(&sentinel).unwrap().lines().count(), 1);
    // A cheaper first run leaves the retry what's left of the day.
    let mut r = rig();
    let (prompt, sentinel) =
        r.launching(r#"{"type":"result","session_id":"s","total_cost_usd":4,"is_error":false}"#);
    assert!(r.spawn(&prompt, None).unwrap().wait().unwrap().success());
    assert!(r.spawn(&prompt, None).unwrap().wait().unwrap().success());
    let argv = fs::read_to_string(&sentinel).unwrap();
    let budgets: Vec<_> = argv
        .lines()
        .map(|l| {
            l.split("--max-budget-usd ")
                .nth(1)
                .unwrap()
                .split(' ')
                .next()
                .unwrap()
        })
        .collect();
    assert_eq!(budgets, ["10.000000", "6.000000"]);
}

/// Admissions take turns: one waits while another holds the lock, so two
/// can't both read the same room before either's marker is written.
#[test]
fn admissions_take_turns_under_one_lock() {
    let r = rig();
    let first = admit(&r.cfg, crate::now()).unwrap();
    assert_eq!(first.budget, 10 * M);
    let (tx, rx) = mpsc::channel();
    let cfg = r.cfg.clone();
    let waiter = std::thread::spawn(move || {
        tx.send(admit(&cfg, crate::now()).map(|a| a.budget))
            .unwrap();
    });
    assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
    // Its run started meanwhile, holding today's $10: the waiter gets none.
    r.turn(
        "first",
        1,
        &[json!({"type":"burrow.run","at":crate::now(),"budget":first.budget})],
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
    let run = |engine| {
        let _ = fs::remove_file(out.with_extension("key"));
        let mut c = Command::new(&stand_in);
        // What it inherits is only what's here: not the test's own session.
        c.env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .envs(inherited);
        let key = credentials(&mut c, engine, &r.cfg).unwrap();
        assert!(c.status().unwrap().success());
        drop(key);
        (
            fs::read_to_string(out.with_extension("names")).unwrap(),
            fs::read_to_string(out.with_extension("key")).ok(),
        )
    };
    let (names, key) = run(Engine::Claude);
    assert_eq!(names, "ANTHROPIC_BASE_URL\nCLAUDE_CODE_OAUTH_TOKEN\n");
    assert_eq!(key, None);
    let (names, key) = run(Engine::ClaudeApi);
    assert_eq!(
        names,
        "ANTHROPIC_BASE_URL\nCLAUDE_CODE_API_KEY_FILE_DESCRIPTOR\n"
    );
    assert_eq!(key.as_deref(), Some(format!("{KEY}\n").as_str()));
    // No key, no start: the refusal names the file, never a key.
    let mut r = r;
    r.cfg.claude_api_key_file = None;
    let mut c = Command::new(&stand_in);
    assert!(credentials(&mut c, Engine::ClaudeApi, &r.cfg).is_err());
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
    assert!(super::super::refused_file(Engine::Claude, last, &stream, 0, 0).is_some());
    assert_eq!(
        super::super::refused_file(Engine::ClaudeApi, last, &stream, 0, 0),
        None
    );
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
