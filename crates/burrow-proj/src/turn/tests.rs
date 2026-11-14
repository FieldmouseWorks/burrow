use std::fs;

use super::clock::gone_quiet;
use super::{github_app, stopped};
use crate::config::Config;
use crate::engine::Engine;

// A Codex turn reaches GitHub as an App, its job's own or the machine's,
// never as the user, whose logins its sandbox hides; with neither it
// doesn't start. Claude's acts as the user unless its job names an App.
#[test]
fn a_codex_turn_acts_as_an_app_or_not_at_all() {
    let mut cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
    let app = |own: Option<&str>, engine| github_app(&cfg, own.map(str::to_owned), engine);
    assert_eq!(app(None, Engine::Codex), Ok(Some("workers".into())));
    assert_eq!(app(Some(""), Engine::Codex), Ok(Some("workers".into())));
    assert_eq!(app(Some("other"), Engine::Codex), Ok(Some("other".into())));
    assert_eq!(app(None, Engine::Claude), Ok(None));
    assert_eq!(app(Some("other"), Engine::Claude), Ok(Some("other".into())));
    for none in [None, Some(String::new())] {
        cfg.machinery = none;
        let app = |own: Option<&str>, engine| github_app(&cfg, own.map(str::to_owned), engine);
        let refused = app(None, Engine::Codex).unwrap_err();
        assert!(refused.contains("only as a GitHub App"), "{refused}");
        assert_eq!(app(Some("other"), Engine::Codex), Ok(Some("other".into())));
        assert_eq!(app(None, Engine::Claude), Ok(None));
    }
}

// A sandbox's VM counts as stopped, everything a refused run left in it
// with it, only once `agent stop` says so: one that fails, or can't run,
// and the turn doesn't go on.
#[test]
fn a_sandbox_stopped_only_when_agent_says_so() {
    let d = tempfile::tempdir().unwrap();
    let agent = |code: i32| {
        let f = d.path().join(format!("agent-{code}"));
        let script = format!(
            "#!/bin/sh\necho \"$*\" >>{}/asked\nexit {code}\n",
            d.path().display()
        );
        burrow_test::executable(&f, script);
        f.to_str().unwrap().to_owned()
    };
    assert!(stopped(&agent(0), "web-ab12"));
    assert!(!stopped(&agent(1), "web-ab12"));
    let missing = d.path().join("no-agent");
    assert!(!stopped(missing.to_str().unwrap(), "web-ab12"));
    let asked = fs::read_to_string(d.path().join("asked")).unwrap();
    assert_eq!(asked, "stop web-ab12\nstop web-ab12\n");
}

const SECOND: i64 = 1_000_000_000;

#[test]
fn quiet_is_all_of_idle_wherever_in_a_second_the_stream_last_spoke() {
    let spoke = 100 * SECOND + 900_000_000;
    // The clock's second turned over fifteen times, in 14.1s of quiet.
    assert!(!gone_quiet(115 * SECOND, spoke, 15 * SECOND));
    assert!(!gone_quiet(spoke + 15 * SECOND - 1, spoke, 15 * SECOND));
    assert!(gone_quiet(spoke + 15 * SECOND, spoke, 15 * SECOND));
}

#[test]
fn no_idle_never_goes_quiet() {
    assert!(!gone_quiet(1000 * SECOND, 0, 0));
}

// A handed-off turn's successor takes over what the old turn registered: the
// quota registration and the question recommendation name it, not the old
// turn, and so aren't dropped as another turn's leftovers.
#[test]
fn a_handoff_retargets_the_waiting_turns_records() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut v = crate::config::tests::example();
    v["jobs"] = tmp.path().to_str().unwrap().into();
    let cfg: Config = serde_json::from_value(v).unwrap();
    fs::create_dir(tmp.path().join("j")).unwrap();
    let job = crate::job::Job::new(&cfg, "j");
    let pending = crate::github_quota::Pending {
        targets: vec!["o/r#5".into()],
        retry_at: 7,
        turn: "1".into(),
        baselines: "b".into(),
    };
    fs::write(
        job.path(crate::github_quota::PENDING),
        serde_json::to_string(&pending).unwrap(),
    )
    .unwrap();
    fs::write(
        job.path("ask.next"),
        r#"{"default":"go","one_way":true,"turn":"1"}"#,
    )
    .unwrap();
    super::handoff::retarget(&job, "1", "2").unwrap();
    let p: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(job.path(crate::github_quota::PENDING)).unwrap())
            .unwrap();
    assert_eq!(
        (
            p["turn"].as_str(),
            p["baselines"].as_str(),
            p["retry_at"].as_u64()
        ),
        (Some("2"), Some("b"), Some(7))
    );
    let a: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(job.path("ask.next")).unwrap()).unwrap();
    assert_eq!(
        (a["turn"].as_str(), a["one_way"].as_bool()),
        (Some("2"), Some(true))
    );
    // Another turn's records are left be.
    super::handoff::retarget(&job, "9", "3").unwrap();
    assert!(
        fs::read_to_string(job.path("ask.next"))
            .unwrap()
            .contains(r#""turn":"2""#)
    );
}

fn handoff_job(tmp: &tempfile::TempDir) -> (Config, String) {
    let mut v = crate::config::tests::example();
    v["jobs"] = tmp.path().to_str().unwrap().into();
    fs::create_dir(tmp.path().join("j")).unwrap();
    (serde_json::from_value(v).unwrap(), "j".into())
}

// A record that can't be moved to the successor is an error, not skipped, and
// the ones already moved are put back, so no turn is accepted that would lose
// its inherited state.
#[test]
fn a_handoff_that_cant_move_every_record_moves_none() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (cfg, id) = handoff_job(&tmp);
    let job = crate::job::Job::new(&cfg, &id);
    let pending = job.path(crate::github_quota::PENDING);
    let first = r#"{"targets":[],"retry_at":1,"turn":"1","baselines":""}"#;
    fs::write(&pending, first).unwrap();
    let second = r#"{"default":"go","one_way":false,"turn":"1"}"#;
    fs::write(job.path("ask.next"), second).unwrap();
    // A planted link at the old staging name is never followed.
    let secret = tmp.path().join("secret");
    fs::write(&secret, "host data").unwrap();
    std::os::unix::fs::symlink(&secret, job.path("ask.next.new")).unwrap();
    super::handoff::retarget(&job, "1", "2").unwrap();
    assert_eq!(fs::read_to_string(&secret).unwrap(), "host data");
    super::handoff::retarget(&job, "2", "1").unwrap();
    fs::remove_file(job.path("ask.next.new")).unwrap();
    // A record or archive that is a link is refused, not copied or followed.
    fs::remove_file(job.path("ask.next")).unwrap();
    std::os::unix::fs::symlink(&secret, job.path("ask.next")).unwrap();
    assert!(super::handoff::retarget(&job, "1", "2").is_err());
    fs::remove_file(job.path("ask.next")).unwrap();
    std::os::unix::fs::symlink(&secret, job.turn("1", "github-awaits")).unwrap();
    assert!(super::handoff::take_over(&job, "1", "2").is_err());
    assert!(!job.turn("2", "github-awaits").exists());
    assert!(!job.turn("2", super::CARRY).exists());
    fs::remove_file(job.turn("1", "github-awaits")).unwrap();
    fs::write(job.path("ask.next"), second).unwrap();
    // One that can't be read is the same.
    fs::write(job.path("ask.next"), "not json").unwrap();
    assert!(super::handoff::retarget(&job, "1", "2").is_err());
    let kept: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&pending).unwrap()).unwrap();
    assert_eq!(kept["turn"], "1");
}

// The old turn's captured awaits go with the records, so the successor's own
// awaits join them and a repeated one keeps its first baseline; giving the
// handoff back removes the copy and leaves the old turn's alone.
#[test]
fn a_handoff_carries_the_captured_awaits() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (cfg, id) = handoff_job(&tmp);
    let job = crate::job::Job::new(&cfg, &id);
    job.set_turns(2).unwrap();
    fs::write(job.turn("1", "github-awaits"), "o/r#1\tbase-a\n").unwrap();
    super::handoff::take_over(&job, "1", "2").unwrap();
    assert_eq!(
        fs::read_to_string(job.turn("2", "github-awaits")).unwrap(),
        "o/r#1\tbase-a\n"
    );
    assert!(job.turn("2", super::CARRY).exists());
    // A successor's await of its own joins; the same target keeps its baseline.
    let joined = crate::github_quota::join_records(
        &crate::github_quota::captured(&job).unwrap(),
        "o/r#1\tother\no/r#2\tbase-b\n",
    );
    assert_eq!(joined, "o/r#1\tbase-a\no/r#2\tbase-b\n");
    super::handoff::give_back(&job, "1", "2").unwrap();
    assert!(!job.turn("2", "github-awaits").exists());
    assert!(!job.turn("2", super::CARRY).exists());
    assert!(job.turn("1", "github-awaits").exists());
}

// A successor that was numbered but never launched or queued is not accepted:
// it's told apart from one that was, by what launching leaves.
#[test]
fn a_numbered_successor_is_accepted_only_once_launched_or_queued() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (mut cfg, id) = handoff_job(&tmp);
    cfg.tools.systemctl = tmp.path().join("systemctl");
    burrow_test::executable(
        &cfg.tools.systemctl,
        "#!/bin/sh\ncase \"$*\" in *LoadState*) echo not-found ;; esac\n",
    );
    let job = crate::job::Job::new(&cfg, &id);
    job.set_turns(2).unwrap();
    use super::handoff::{Fate, fate};
    assert_eq!(fate(&cfg, &job, "2"), Fate::Unaccepted);
    // Recovery's synthetic outcome for an unlaunched successor proves nothing.
    fs::write(job.turn("2", "outcome"), "failed\n").unwrap();
    assert_eq!(fate(&cfg, &job, "2"), Fate::Unaccepted);
    // A unit that can't be asked about is unknown, never absent.
    burrow_test::executable(&cfg.tools.systemctl, "#!/bin/sh\nexit 1\n");
    assert_eq!(fate(&cfg, &job, "2"), Fate::Unknown);
    burrow_test::executable(
        &cfg.tools.systemctl,
        "#!/bin/sh\ncase \"$*\" in *LoadState*) echo not-found ;; esac\n",
    );
    fs::write(job.turn("2", "slot"), "").unwrap();
    assert_eq!(fate(&cfg, &job, "2"), Fate::Accepted);
    fs::remove_file(job.turn("2", "slot")).unwrap();
    fs::write(
        crate::policy::path(&job, "park"),
        r#"{"state":"queued","turn":"2","first":false,"at":0}"#,
    )
    .unwrap();
    assert_eq!(fate(&cfg, &job, "2"), Fate::Accepted);
    // Giving the unaccepted one up puts the job back at the old turn.
    fs::remove_file(crate::policy::path(&job, "park")).unwrap();
    fs::write(job.path("prompt.2"), "p").unwrap();
    super::handoff::abandon(&job, "1", "2").unwrap();
    assert_eq!(job.turns().as_deref(), Some("1"));
    assert!(!job.path("prompt.2").exists());
}
