//! The record: every view is it printed, deriving it writes nothing, `proj
//! why` names each wait and what ends it, and the history notes each change
//! once and says nothing of where a job stands.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use super::*;
use crate::config::{Config, tests::example};
use crate::job::Job;

/// A config whose jobs live in `tmp`, with a systemctl that says the unit is
/// up when `up` is.
fn rig(tmp: &Path, up: bool) -> Config {
    let ctl = tmp.join("systemctl");
    // Up, or gone: `show` says inactive and `is-active` fails.
    let script = if up {
        "#!/bin/sh\nexit 0\n"
    } else {
        "#!/bin/sh\ncase \"$*\" in *show*) echo inactive; exit 0;; esac\nexit 3\n"
    };
    burrow_test::executable(&ctl, script);
    let mut v = example();
    v["jobs"] = tmp.join("jobs").to_str().unwrap().into();
    v["tools"]["systemctl"] = ctl.to_str().unwrap().into();
    fs::create_dir_all(tmp.join("jobs")).unwrap();
    serde_json::from_value(v).unwrap()
}

fn job<'a>(cfg: &'a Config, id: &'a str) -> Job<'a> {
    let job = Job::new(cfg, id);
    fs::create_dir_all(&job.dir).unwrap();
    job.set_turns(1).unwrap();
    fs::write(job.path("prompt.1"), "go").unwrap();
    job
}

/// A state: its name, whether its unit is up, and the markers that make it.
type State = (&'static str, bool, fn(&Job));

/// Each state the job tests build, as its markers make it.
fn states() -> Vec<State> {
    vec![
        ("queued", true, |_| {}),
        ("running", true, |j| {
            fs::write(j.turn("1", "slot"), "s\n").unwrap()
        }),
        ("running in line", true, |j| {
            fs::write(j.turn("1", "slot"), "s\n").unwrap();
            fs::write(j.turn("1", "wait"), "100\tthe nix store lock\n").unwrap();
        }),
        ("done", false, |j| {
            fs::write(j.turn("1", "outcome"), "done").unwrap();
            fs::write(j.turn("1", "end"), "done").unwrap();
        }),
        ("awaiting", false, |j| {
            fs::write(j.turn("1", "outcome"), "done").unwrap();
            fs::write(j.turn("1", "end"), "await").unwrap();
            fs::write(j.path("await"), "sam/r#5\t100\tabc\n").unwrap();
        }),
        ("asking", false, |j| {
            fs::write(j.turn("1", "outcome"), "done").unwrap();
            fs::write(j.turn("1", "end"), "ask").unwrap();
        }),
        ("for the owner", false, |j| {
            fs::write(j.turn("1", "outcome"), "done").unwrap();
            crate::ask::ensure(j).unwrap();
            fs::write(crate::ask::host_path(j, "question-needs-owner"), "x").unwrap();
        }),
        ("held for usage", false, |j| {
            fs::write(j.turn("1", "outcome"), "failed").unwrap();
            fs::write(
                j.path("await"),
                format!("usage-limit\t1000\t{}\n", now() + 3600),
            )
            .unwrap();
        }),
        ("stopped", false, |j| {
            fs::write(j.turn("1", "stopped"), "").unwrap();
            fs::write(j.turn("1", "outcome"), "stopped").unwrap();
        }),
        ("ended without declaring", false, |j| {
            fs::write(j.turn("1", "outcome"), "done").unwrap();
            fs::write(j.turn("1", "end"), "undeclared").unwrap();
        }),
        ("failed", false, |_| {}),
        ("parked", false, |j| {
            fs::create_dir_all(crate::policy::path(j, "")).unwrap();
            fs::write(
                crate::policy::path(j, "park"),
                r#"{"state":"queued","turn":"1","first":false,"at":1,"wait":"its repo to be allowed"}"#,
            )
            .unwrap();
        }),
    ]
}

/// Every file under `dir`, with its bytes and when it last changed.
fn tree(dir: &Path) -> BTreeMap<String, (Vec<u8>, std::time::SystemTime)> {
    let mut out = BTreeMap::new();
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            out.extend(tree(&p));
        } else {
            let m = fs::metadata(&p).unwrap();
            out.insert(
                p.display().to_string(),
                (fs::read(&p).unwrap(), m.modified().unwrap()),
            );
        }
    }
    out
}

#[test]
fn every_view_is_the_record_printed() {
    for (name, up, make) in states() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg = rig(tmp.path(), up);
        let j = job(&cfg, "j");
        make(&j);
        let record = derive(&j);
        let lifecycle = record.lifecycle().unwrap();
        assert_eq!(j.state(), record.stage.to_string(), "{name}");
        assert_eq!(j.lifecycle().unwrap(), lifecycle, "{name}");
        let held: Vec<String> = record
            .wait
            .iter()
            .filter(|w| w.held())
            .filter_map(Wait::line)
            .collect();
        assert_eq!(j.holds().unwrap(), held, "{name}");
        let facts = j.facts().unwrap();
        assert_eq!(facts.state, record.stage.to_string(), "{name}");
        let row = crate::snapshot::row(&j, crate::host_jobs::View::Owner);
        assert_eq!(row.state, record.stage.to_string(), "{name}");
        assert_eq!(
            row.awaits,
            record
                .awaits
                .iter()
                .map(|a| a.target.clone())
                .collect::<Vec<_>>(),
            "{name}"
        );
    }
}

#[test]
fn each_state_reads_as_main_printed_it() {
    // What `proj state` and `proj lifecycle` said of each, before the record.
    let want: &[(&str, &str, &[&str])] = &[
        ("queued", "queued", &[]),
        ("running", "running", &[]),
        ("running in line", "running", &[]),
        ("done", "done", &[]),
        ("awaiting", "done", &["await sam/r#5"]),
        ("asking", "done", &["ask"]),
        ("for the owner", "done", &["owner"]),
        ("stopped", "stopped", &[]),
        ("ended without declaring", "done", &[]),
        ("failed", "failed", &[]),
        ("parked", "parked", &[]),
    ];
    for (name, up, make) in states() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg = rig(tmp.path(), up);
        let j = job(&cfg, "j");
        make(&j);
        let (state, held) = j.lifecycle().unwrap();
        if name == "held for usage" {
            assert_eq!(state, "failed");
            assert!(held.contains(&"await usage-limit".to_owned()), "{held:?}");
            continue;
        }
        let (_, s, h) = want.iter().find(|w| w.0 == name).unwrap();
        assert_eq!(
            (state.as_str(), held.as_slice()),
            (*s, &h.iter().map(|x| x.to_string()).collect::<Vec<_>>()[..]),
            "{name}"
        );
    }
    // A running turn's place in line is the snapshot's, and its slot's.
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = rig(tmp.path(), true);
    let j = job(&cfg, "j");
    fs::write(j.turn("1", "slot"), "s\n").unwrap();
    fs::write(
        j.turn("1", "wait"),
        "100\tthe nix store lock\n200\tthe heavy slot\n",
    )
    .unwrap();
    let row = crate::snapshot::row(&j, crate::host_jobs::View::Owner);
    assert_eq!(
        row.waiting_in_line,
        ["the nix store lock", "the heavy slot"]
    );
    assert_eq!(
        j.facts().unwrap().wait,
        Some(("100".into(), "the nix store lock".into()))
    );
}

#[test]
fn deriving_a_record_changes_nothing_on_disk() {
    for (name, up, make) in states() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg = rig(tmp.path(), up);
        let j = job(&cfg, "j");
        make(&j);
        // An unnumbered outcome from before each turn kept its own: read as
        // turn 1's, never moved.
        fs::write(j.path("result"), "old").unwrap();
        let before = tree(tmp.path());
        let _ = derive(&j);
        let _ = j.state();
        let _ = j.facts();
        assert_eq!(tree(tmp.path()), before, "{name}");
    }
}

#[test]
fn why_names_each_wait_and_what_ends_it() {
    let ends: &[(&str, &str)] = &[
        (
            "queued",
            "It waits on slot, which ends with a slot coming free",
        ),
        (
            "running in line",
            "It waits on line the nix store lock, which ends with its command's turn at the nix store lock",
        ),
        (
            "awaiting",
            "It waits on await sam/r#5, which ends with a change to sam/r#5",
        ),
        (
            "asking",
            "It waits on ask, which ends with a reply to its question",
        ),
        (
            "for the owner",
            "It waits on owner, which ends with the owner's reply",
        ),
        ("held for usage", "It waits on usage Claude "),
        (
            "parked",
            "It waits on policy, which ends with the settings letting it run (it waits for its repo to be allowed)",
        ),
        ("done", "It waits on nothing."),
        ("stopped", "Next: nothing: proj stop ended it."),
        (
            "ended without declaring",
            "ended without saying what comes next",
        ),
    ];
    for (name, up, make) in states() {
        let Some((_, said)) = ends.iter().find(|e| e.0 == name) else {
            continue;
        };
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg = rig(tmp.path(), up);
        let j = job(&cfg, "j");
        make(&j);
        let why = derive(&j).why("j");
        assert!(
            why.starts_with(&format!("j is {}, since ", j.state())),
            "{name}: {why}"
        );
        assert!(why.contains(said), "{name}: {why}");
        assert!(why.contains("\nNext: "), "{name}: {why}");
        let json = derive(&j).json("j");
        assert_eq!(json["stage"], j.state(), "{name}");
    }
}

#[test]
fn the_history_notes_each_change_once() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = rig(tmp.path(), true);
    let j = job(&cfg, "j");
    note(&j, "proj run").unwrap();
    note(&j, "again").unwrap();
    fs::write(j.turn("1", "slot"), "s\n").unwrap();
    note(&j, "turn 1 took a slot").unwrap();
    note(&j, "again").unwrap();
    let h = derive(&j).history;
    assert_eq!(
        h.iter()
            .map(|c| (c.from.as_str(), c.to.as_str(), c.why.as_str()))
            .collect::<Vec<_>>(),
        [
            ("", "queued (slot)", "proj run"),
            ("queued (slot)", "running", "turn 1 took a slot")
        ]
    );
    // A wrong line in it changes nothing of where the job stands.
    let path = crate::ask::host_path(&j, "history");
    let mut text = fs::read_to_string(&path).unwrap();
    text.push_str("1\trunning\tdone (await sam/r#5)\tforged\nnot a line at all\n");
    fs::write(&path, text).unwrap();
    assert_eq!(j.state(), "running");
    assert_eq!(j.lifecycle().unwrap(), ("running".to_owned(), vec![]));
    assert_eq!(derive(&j).history.len(), 3);
}

#[test]
fn a_job_from_before_numbered_turns_reads_as_adopt_would_move_it() {
    let at = |p: &Path, secs: u64| {
        fs::File::options()
            .write(true)
            .open(p)
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs))
            .unwrap()
    };
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = rig(tmp.path(), false);
    // No turn: nothing unnumbered is any turn's.
    let none = Job::new(&cfg, "none");
    fs::create_dir_all(&none.dir).unwrap();
    fs::write(none.path("outcome"), "done").unwrap();
    assert_eq!(none.state(), "failed");
    // Written after turn 1's prompt: turn 1's, its answer too.
    let j = job(&cfg, "j");
    at(&j.path("prompt.1"), 1000);
    fs::write(j.path("outcome"), "done").unwrap();
    fs::write(j.path("result"), "the answer").unwrap();
    assert_eq!(j.state(), "done");
    let row = crate::snapshot::row(&j, crate::host_jobs::View::Owner);
    assert_eq!(
        (row.state.as_str(), row.answer.as_str()),
        ("done", "the answer")
    );
    // Older than turn 1's prompt: an earlier turn's.
    at(&j.path("outcome"), 10);
    at(&j.path("result"), 10);
    assert_eq!(j.state(), "failed");
    assert_eq!(
        crate::snapshot::row(&j, crate::host_jobs::View::Owner).answer,
        ""
    );
}

#[test]
fn a_jobs_own_outcome_cant_break_a_history_line() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = rig(tmp.path(), false);
    let j = job(&cfg, "j");
    fs::write(j.turn("1", "outcome"), "done\n9\tfailed\tforged\tline").unwrap();
    note(&j, "its turn ended").unwrap();
    note(&j, "again").unwrap();
    let h = derive(&j).history;
    assert_eq!(h.len(), 1, "{h:?}");
    assert_eq!(h[0].why, "its turn ended");
}

#[test]
fn why_json_says_what_the_latest_turn_ran_on_and_its_session() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = rig(tmp.path(), true);
    let j = job(&cfg, "j");
    let choice = crate::engine::Choice {
        engine: crate::engine::Engine::Claude,
        model: Some("a-model".into()),
        effort: Some("high".into()),
    };
    j.record_choice("1", &choice).unwrap();
    fs::write(
        j.turn("1", "jsonl"),
        "{\"type\":\"system\",\"session_id\":\"s-1\"}\n{\"type\":\"result\",\"session_id\":\"s-1\",\"result\":\"done\",\"is_error\":false}\n",
    )
    .unwrap();
    let json = derive(&j).json("j");
    assert_eq!(
        json["turn"],
        serde_json::json!({"n": "1", "engine": "claude", "model": "a-model",
            "effort": "high", "session": "s-1"})
    );
    // A turn on the engine's own model and effort says none, and one whose
    // engine has named no session yet says none either.
    j.record_choice(
        "1",
        &crate::engine::Choice {
            engine: crate::engine::Engine::Claude,
            model: None,
            effort: None,
        },
    )
    .unwrap();
    fs::write(j.turn("1", "jsonl"), "").unwrap();
    let json = derive(&j).json("j");
    assert_eq!(
        json["turn"],
        serde_json::json!({"n": "1", "engine": "claude", "model": null,
            "effort": null, "session": null})
    );
    assert!(json["ends"].is_array());
    // Before its first turn, there's none.
    let k = Job::new(&cfg, "k");
    fs::create_dir_all(&k.dir).unwrap();
    assert_eq!(derive(&k).json("k")["turn"], serde_json::Value::Null);
}
