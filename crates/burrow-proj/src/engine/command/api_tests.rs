//! Launch integration tests for `claude-api` admission through `spawn`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use burrow_engine_anthropic::api::Refused;
use burrow_settings::credit::ENGINE;
use serde_json::json;

use crate::config::{
    Config,
    tests::{example, set},
};
use crate::engine::{Choice, Engine, spawn};
use crate::job::Job;

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
