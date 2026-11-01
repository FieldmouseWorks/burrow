//! `end`, as the unit's ExecStopPost has it, and the sweep of worktrees a
//! run left: `end.json` says done only for a run that exited 0 and kept
//! its verdicts, timed out past RuntimeMaxSec, failed otherwise; the run's
//! worktrees go, other runs' stay; the repo's pass is kicked.

mod world;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;
use std::thread;

use burrow_proof::end::{Ended, end};
use burrow_proof::run::run;
use burrow_proof::trees::sweep;
use serde_json::{Value, json};
use world::{Plain, World, git, read, which};

fn ended(result: &str, code: &str, status: &str) -> Ended {
    Ended {
        result: result.into(),
        code: code.into(),
        status: status.into(),
    }
}

fn end_json(dir: &Path) -> Value {
    let mut v: Value = serde_json::from_str(&read(&dir.join("end.json"))).unwrap();
    assert!(v["at"].as_f64().is_some_and(|t| t > 1e9), "{v}");
    v.as_object_mut().unwrap().remove("at");
    v
}

/// A worktree of the run ID's, as one killed mid-command leaves it.
fn left(w: &World, id: &str, i: usize) {
    let tree = w.trees.join(format!("{id}-{i}"));
    git(&w.checkout, &["fetch", "-q", "origin", "refs/pull/5/head"]);
    git(
        &w.checkout,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            tree.to_str().unwrap(),
            &w.head,
        ],
    );
    // One its command made read-only goes too.
    fs::create_dir(tree.join("ro")).unwrap();
    fs::write(tree.join("ro/x"), "x").unwrap();
    fs::set_permissions(
        tree.join("ro"),
        std::os::unix::fs::PermissionsExt::from_mode(0o555),
    )
    .unwrap();
}

#[test]
fn done_only_when_it_exited_0_and_kept_its_verdicts() {
    let w = World::new();
    let dir = w.made(
        "proof-sam-r-5-1",
        &w.record(&[("a", "true"), ("b", "exit 4")]),
    );
    run(&dir, &mut Plain::default()).unwrap();
    // The shape the steward reads (proofs.py verdicts, common.py build_state).
    // What it returns is what it wrote (its time aside: a float that
    // needn't read back to the last digit).
    let mut said = end(&dir, &ended("success", "exited", "0")).unwrap();
    said.as_object_mut().unwrap().remove("at");
    assert_eq!(said, end_json(&dir));
    assert_eq!(
        end_json(&dir),
        json!({"state": "done", "exit": 0,
               "verdicts": {"a": ["success", "Ran here and passed"], "b": ["failure", "Ran here and failed: exit 4"]},
               "exits": {"a": 0, "b": 4}})
    );
    for (how, state, exit) in [
        // Stopped: SIGTERM is a clean end to systemd, but it's no verdict.
        (ended("success", "killed", "TERM"), "failed", Value::Null),
        (ended("success", "exited", "1"), "failed", json!(1)),
        (ended("exit-code", "exited", "1"), "failed", json!(1)),
        (ended("signal", "killed", "KILL"), "failed", Value::Null),
        (ended("timeout", "killed", "TERM"), "timed out", Value::Null),
    ] {
        end(&dir, &how).unwrap();
        assert_eq!(
            end_json(&dir),
            json!({"state": state, "exit": exit}),
            "{how:?}"
        );
    }
    // Exited 0 with no verdicts kept: it ended short.
    fs::remove_file(dir.join("ran.json")).unwrap();
    end(&dir, &ended("success", "exited", "0")).unwrap();
    assert_eq!(end_json(&dir)["state"], "failed");
}

#[test]
fn its_worktrees_go_and_other_runs_stay() {
    let w = World::new();
    let dir = w.made("proof-sam-r-5-1", &w.record(&[("gate", "true")]));
    w.made("proof-sam-r-5-12", &w.record(&[("gate", "true")]));
    left(&w, "proof-sam-r-5-1", 0);
    left(&w, "proof-sam-r-5-1", 1);
    left(&w, "proof-sam-r-5-12", 0);
    end(&dir, &ended("timeout", "killed", "TERM")).unwrap();
    assert_eq!(w.trees_left(), ["proof-sam-r-5-12-0"]);
    assert_eq!(w.worktrees().len(), 2, "{:?}", w.worktrees());
    // One whose cgroup is gone has nothing of its fences left to clear.
    fs::write(
        dir.join("cgroup"),
        w.path().join("no-such-cgroup").to_str().unwrap(),
    )
    .unwrap();
    end(&dir, &ended("success", "killed", "TERM")).unwrap();
    assert_eq!(end_json(&dir)["state"], "failed");
}

#[test]
fn a_run_with_no_record_still_ends() {
    let w = World::new();
    let dir = w.builds.join("proof-sam-r-5-1");
    fs::create_dir_all(&dir).unwrap();
    end(&dir, &ended("exit-code", "exited", "203")).unwrap();
    assert_eq!(end_json(&dir), json!({"state": "failed", "exit": 203}));
}

#[test]
fn it_kicks_the_repos_pass() {
    let w = World::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let heard = thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(&s).read_line(&mut line).unwrap();
        s.write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
            .unwrap();
        line
    });
    let mut rec = w.record(&[("gate", "true")]);
    rec.kick = Some(format!("http://127.0.0.1:{port}/kick/autoland/sam/r"));
    let dir = w.made("proof-sam-r-5-1", &rec);
    end(&dir, &ended("timeout", "killed", "TERM")).unwrap();
    assert_eq!(
        heard.join().unwrap().trim(),
        "POST /kick/autoland/sam/r HTTP/1.1"
    );
    // One that can't be heard is no reason not to end.
    rec.kick = Some("http://127.0.0.1:9/kick/autoland/sam/r".into());
    let dir = w.made("proof-sam-r-5-2", &rec);
    end(&dir, &ended("timeout", "killed", "TERM")).unwrap();
    assert_eq!(end_json(&dir)["state"], "timed out");
}

#[test]
fn the_sweep_takes_only_what_no_process_can_still_be_in() {
    let w = World::new();
    let rec = w.record(&[("gate", "true")]);
    let live_cgroup = w.path().join("live.service");
    fs::create_dir(&live_cgroup).unwrap();
    // Over: an end.json; a cgroup that's gone; no cgroup recorded at all.
    // Live: its cgroup is still there.
    for (id, cgroup, over) in [
        ("proof-ended", Some(live_cgroup.as_path()), true),
        (
            "proof-gone",
            Some(w.path().join("gone.service").as_path()),
            false,
        ),
        ("proof-unrecorded", None, false),
        ("proof-live", Some(live_cgroup.as_path()), false),
    ] {
        let dir = w.made(id, &rec);
        if let Some(c) = cgroup {
            fs::write(dir.join("cgroup"), c.to_str().unwrap()).unwrap();
        }
        if over {
            fs::write(dir.join("end.json"), "{}").unwrap();
        }
        left(&w, id, 0);
    }
    // And one whose folder's long gone (pruned).
    left(&w, "proof-pruned", 0);
    assert!(sweep(&w.trees, &w.builds, "proof-me", &which("git")).is_empty());
    assert_eq!(w.trees_left(), ["proof-live-0"]);
    git(&w.checkout, &["worktree", "prune"]);
    assert_eq!(w.worktrees().len(), 2, "{:?}", w.worktrees());
    // A run sweeps before it runs.
    fs::remove_dir(&live_cgroup).unwrap();
    let dir = w.made("proof-me", &rec);
    run(&dir, &mut Plain::default()).unwrap();
    assert!(
        w.trees_left().is_empty() && w.worktrees().len() == 1,
        "{:?}",
        w.worktrees()
    );
}
