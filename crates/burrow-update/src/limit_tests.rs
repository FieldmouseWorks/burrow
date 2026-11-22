//! The limit as the kernel keeps it. It kills the process it's in, so each
//! test plays the run in a process of its own: this binary again, at that
//! test alone. tests/update-limit.nix holds the same to a real systemd,
//! reloads included.

use std::os::unix::process::ExitStatusExt as _;
use std::process::{Command, Output, Stdio};
use std::{env, fs};

use burrow_github::fake::Fake;

use super::*;
use crate::testing::{Machine, bin, lock, sha};

/// Set in the process that plays the run.
const PLAYS: &str = "BURROW_UPDATE_TEST_PLAYS_THE_RUN";

/// The longest a played run takes: one still going is past any limit here,
/// and a stopped one would never end.
const BOUND: Duration = Duration::from_secs(120);

/// `run`, in a process of its own for the test `name`: how that ended, and
/// after how long. Nothing in that process, which is the one running it.
fn played(name: &str, run: impl FnOnce()) -> Option<(Output, Duration)> {
    if env::var_os(PLAYS).is_some() {
        run();
        return None;
    }
    // What a killed one leaves goes with this.
    let tmp = tempfile::tempdir().unwrap();
    let began = Instant::now();
    let child = Command::new(env::current_exe().unwrap())
        .args(["--exact", &format!("limit::tests::{name}"), "--nocapture"])
        .env(PLAYS, "1")
        .env("TMPDIR", tmp.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let pid = nix::unistd::Pid::from_raw(child.id() as i32);
    let (done, ended) = mpsc::channel();
    thread::spawn(move || done.send(child.wait_with_output()));
    let Ok(out) = ended.recv_timeout(BOUND) else {
        let _ = nix::sys::signal::kill(pid, Signal::SIGKILL);
        panic!("{name}: still going after {BOUND:?}: nothing ended it");
    };
    Some((out.unwrap(), began.elapsed()))
}

fn killed(out: &Output) -> bool {
    out.status.signal() == Some(Signal::SIGKILL as i32)
}

/// A wait of `wait` for a slot.
fn slot_after(limit: &Limit, wait: Duration) {
    let (said, mut says) = std::io::pipe().unwrap();
    let heavy = thread::spawn(move || {
        thread::sleep(wait);
        std::io::Write::write_all(&mut says, format!("{IN_SLOT}\n").as_bytes()).unwrap();
    });
    limit.wait(said);
    heavy.join().unwrap();
}

fn limited(work: Duration) -> Limit {
    let mut m = Machine::new();
    m.cfg.work_limit = Some(work);
    Limit::begin(&m.cfg).ok().unwrap()
}

#[test]
fn an_update_that_waits_longer_than_its_work_limit_isnt_killed_for_it() {
    let work = Duration::from_secs(20);
    let name = "an_update_that_waits_longer_than_its_work_limit_isnt_killed_for_it";
    let Some((out, took)) = played(name, || {
        let mut m = Machine::running_main(&[("nixpkgs", &sha('a')), ("burrow", &sha('b'))]);
        m.cfg.heavy = Some(bin().join("heavy"));
        m.cfg.work_limit = Some(work);
        m.landed(
            "flake.lock",
            &lock(&[("nixpkgs", &sha('c')), ("burrow", &sha('b'))]),
        );
        fs::write(m.home("in-slot"), "sleep 25").unwrap();
        assert_eq!(crate::run::once(&m.cfg, &mut Fake::default(), false), 0);
    }) else {
        return;
    };
    assert!(out.status.success(), "{out:?}");
    assert!(took > work, "it waited past its limit: {took:?}");
}

#[test]
fn each_wait_comes_back_and_the_work_between_them_doesnt() {
    let name = "each_wait_comes_back_and_the_work_between_them_doesnt";
    let Some((out, _)) = played(name, || {
        let limit = limited(Duration::from_secs(12));
        slot_after(&limit, Duration::from_secs(4));
        thread::sleep(Duration::from_secs(6));
        slot_after(&limit, Duration::from_secs(4));
        // 6 of its 12 left: none, were the waits counted, and all 12, were
        // the work between them given back too.
        thread::sleep(Duration::from_secs(1));
        eprintln!("both waits came back");
        thread::sleep(Duration::from_secs(7));
        eprintln!("the work came back");
    }) else {
        return;
    };
    // By what it got to say, not a clock that a loaded machine stretches.
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(
        said.contains("both waits came back"),
        "killed for waiting: {out:?}"
    );
    assert!(
        !said.contains("the work came back"),
        "worked past it: {out:?}"
    );
    assert!(killed(&out), "{out:?}");
}

#[test]
fn work_past_the_limit_is_killed() {
    let Some((out, _)) = played("work_past_the_limit_is_killed", || {
        let _limit = limited(Duration::from_secs(1));
        thread::sleep(Duration::from_secs(60));
    }) else {
        return;
    };
    assert!(killed(&out), "{out:?}");
}

#[test]
fn a_stopped_run_is_killed_at_its_limit() {
    let Some((out, _)) = played("a_stopped_run_is_killed_at_its_limit", || {
        let _limit = limited(Duration::from_secs(1));
        nix::sys::signal::raise(Signal::SIGSTOP).unwrap();
    }) else {
        return;
    };
    assert!(killed(&out), "{out:?}");
}

#[test]
fn a_run_with_nothing_left_is_killed_not_let_off() {
    let Some((out, _)) = played("a_run_with_nothing_left_is_killed_not_let_off", || {
        let _limit = limited(Duration::ZERO);
        thread::sleep(Duration::from_secs(60));
    }) else {
        return;
    };
    assert!(killed(&out), "{out:?}");
}

#[test]
fn a_heavy_that_ends_without_a_slot_ends_the_wait() {
    let limit = Limit::begin(&Machine::new().cfg).ok().unwrap();
    let began = Instant::now();
    limit.wait(&b"not a slot\n"[..]);
    assert!(began.elapsed() < Duration::from_secs(10));
}
