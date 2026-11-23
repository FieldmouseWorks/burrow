//! The binary as `heavy` runs it, on slots of its own. The module's whole
//! tool, as it's wrapped, is held to the rest by the `cpu` check (checks/cpu.nix).

use std::fs::{self, File};
use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use rustix::fs::{FlockOperation, flock};
use tempfile::TempDir;

struct Slots {
    dir: TempDir,
    config: PathBuf,
}

impl Slots {
    fn new(slots: u32, grace: u64) -> Slots {
        let dir = tempfile::tempdir().unwrap();
        for i in 1..=slots {
            File::create(dir.path().join(format!("slot{i}"))).unwrap();
        }
        File::create(dir.path().join("all")).unwrap();
        let config = dir.path().join("heavy.json");
        fs::write(
            &config,
            serde_json::json!({
                "slots": slots,
                "grace": grace,
                "dir": dir.path(),
                "setsid": which("setsid"),
            })
            .to_string(),
        )
        .unwrap();
        Slots { dir, config }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn heavy(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(burrow_test::program!("burrow-heavy"));
        cmd.arg(&self.config)
            .args(args)
            .current_dir(self.path())
            .env_remove("HEAVY_SLOT")
            .env_remove("CARGO_BUILD_JOBS")
            .env_remove("RUST_TEST_THREADS")
            .env_remove("NIX_CONFIG");
        cmd
    }

    /// Runs heavy to the end, failing after 30 seconds.
    fn run(&self, args: &[&str]) -> Output {
        let mut child = self
            .heavy(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        exits(&mut child);
        child.wait_with_output().unwrap()
    }

    /// Makes heavy start its commands through a setsid that waits half a
    /// second first, having touched `started`: until it runs, the command
    /// is heavy's fork, in heavy's process group.
    fn slow_setsid(&self) {
        let wrapper = self.path().join("slow-setsid");
        burrow_test::executable(
            &wrapper,
            format!(
                "#!/bin/sh\ntouch started\nsleep 0.5\nexec {} \"$@\"\n",
                which("setsid").display()
            ),
        );
        let mut config: serde_json::Value =
            serde_json::from_slice(&fs::read(&self.config).unwrap()).unwrap();
        config["setsid"] = wrapper.to_str().unwrap().into();
        fs::write(&self.config, config.to_string()).unwrap();
    }

    /// Holds slot `i` the way a run does, until the file is dropped.
    fn hold(&self, i: u32) -> File {
        let file = File::open(self.path().join(format!("slot{i}"))).unwrap();
        flock(&file, FlockOperation::NonBlockingLockExclusive).unwrap();
        file
    }

    fn status(&self) -> String {
        text(&self.run(&["status"]).stdout)
    }
}

fn which(name: &str) -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|d| d.join(name))
        .find(|p| p.is_file())
        .unwrap_or_else(|| panic!("no {name} on PATH"))
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// Waits for `what` to hold, failing after 30 seconds.
fn until(what: &str, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(30);
    while !f() {
        assert!(Instant::now() < end, "not {what} after 30s");
        thread::sleep(Duration::from_millis(10));
    }
}

/// Waits for `child` to exit, killing it and failing after 30 seconds, so
/// a heavy that never exits fails the test instead of hanging it.
fn exits(child: &mut Child) -> ExitStatus {
    let mut status = None;
    let end = Instant::now() + Duration::from_secs(30);
    while status.is_none() {
        if Instant::now() >= end {
            let _ = child.kill();
            let _ = child.wait();
            panic!("heavy still running after 30s");
        }
        thread::sleep(Duration::from_millis(10));
        status = child.try_wait().unwrap();
    }
    status.unwrap()
}

fn threads() -> u32 {
    let out = Command::new("nproc").output().unwrap();
    text(&out.stdout).trim().parse().unwrap()
}

#[test]
fn a_config_it_doesnt_know_is_refused() {
    let s = Slots::new(1, 3);
    let mut config: serde_json::Value =
        serde_json::from_slice(&fs::read(&s.config).unwrap()).unwrap();
    config["slot"] = 2.into();
    fs::write(&s.config, config.to_string()).unwrap();
    let out = s.run(&["--", "true"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("unknown field `slot`"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn status_says_which_slots_are_free() {
    let s = Slots::new(3, 3);
    fs::remove_file(s.path().join("slot3")).unwrap();
    let held = s.hold(2);
    let share = (threads() / 3).max(1);
    assert_eq!(
        s.status(),
        format!(
            "3 slots of {share} threads (of {})\nslot1: free\nslot2: busy\nslot3: missing\n",
            threads()
        )
    );
    drop(held);
    assert!(s.status().contains("slot2: free"));
}

/// What `heavy status` and nproc count with these OpenMP variables set.
fn counts(s: &Slots, omp: &[(&str, &str)]) -> (String, String) {
    let set = |mut cmd: Command| {
        cmd.env_remove("OMP_NUM_THREADS")
            .env_remove("OMP_THREAD_LIMIT");
        cmd.envs(omp.iter().copied()).output().unwrap()
    };
    let status = text(&set(s.heavy(&["status"])).stdout);
    let of = status
        .lines()
        .next()
        .and_then(|l| l.split_once("(of "))
        .map(|(_, n)| n.trim_end_matches(')').to_owned())
        .unwrap_or_else(|| panic!("no count in {status:?}"));
    (
        of,
        text(&set(Command::new("nproc")).stdout).trim().to_owned(),
    )
}

#[test]
fn it_counts_the_threads_as_nproc_does() {
    let s = Slots::new(2, 3);
    for omp in [
        &[][..],
        &[("OMP_NUM_THREADS", "3")],
        &[("OMP_NUM_THREADS", "4096")],
        &[("OMP_NUM_THREADS", " 5,2 ")],
        &[("OMP_NUM_THREADS", "0")],
        &[("OMP_NUM_THREADS", "two")],
        &[("OMP_THREAD_LIMIT", "1")],
        &[("OMP_THREAD_LIMIT", "2")],
        &[("OMP_THREAD_LIMIT", "4096")],
        &[("OMP_THREAD_LIMIT", "0")],
        &[("OMP_NUM_THREADS", "8"), ("OMP_THREAD_LIMIT", "3")],
        &[("OMP_NUM_THREADS", "x"), ("OMP_THREAD_LIMIT", "2")],
    ] {
        let (heavy, nproc) = counts(&s, omp);
        assert_eq!(heavy, nproc, "{omp:?}");
    }
    // And the share follows the count.
    let out = s
        .heavy(&["sh", "-c", "echo $HEAVY_JOBS"])
        .env("OMP_NUM_THREADS", "6")
        .output()
        .unwrap();
    assert_eq!(text(&out.stdout), "3\n");
}

#[test]
fn the_command_gets_its_share_and_its_status_passes_through() {
    let s = Slots::new(2, 3);
    let share = (threads() / 2).max(1).to_string();
    let out = s.run(&[
        "--",
        "sh",
        "-c",
        "echo $HEAVY_SLOT $HEAVY_JOBS $CARGO_BUILD_JOBS $RUST_TEST_THREADS; exit 7",
    ]);
    assert_eq!(out.status.code(), Some(7));
    assert_eq!(text(&out.stdout), format!("1 {share} {share} {share}\n"));
    // The second slot when the first is held; a job count already set is kept.
    let _held = s.hold(1);
    let out = s
        .heavy(&["sh", "-c", "echo $HEAVY_SLOT $CARGO_BUILD_JOBS"])
        .env("CARGO_BUILD_JOBS", "3")
        .output()
        .unwrap();
    assert_eq!(text(&out.stdout), "2 3\n");
    // A command that couldn't start, as a shell says it: run in a slot,
    // and inside one, where heavy execs it itself.
    fs::write(s.path().join("not-executable"), "").unwrap();
    for (command, status) in [("./no-such-command", 127), ("./not-executable", 126)] {
        assert_eq!(s.run(&["--", command]).status.code(), Some(status));
        let out = s
            .heavy(&["--", command])
            .env("HEAVY_SLOT", "1")
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(status), "{command} inside a slot");
    }
}

#[test]
fn the_command_reads_the_callers_stdin() {
    let s = Slots::new(1, 3);
    let mut child = s
        .heavy(&["--", "cat"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"7\n").unwrap();
    exits(&mut child);
    assert_eq!(text(&child.wait_with_output().unwrap().stdout), "7\n");
}

#[test]
fn a_call_it_cant_read_is_a_usage_error() {
    let s = Slots::new(1, 3);
    for bad in [&[][..], &["--"], &["--wait", "soon", "--", "true"]] {
        let out = s.run(bad);
        assert_eq!(out.status.code(), Some(2), "{bad:?}");
        assert!(text(&out.stderr).starts_with("usage: heavy "), "{bad:?}");
    }
}

#[test]
fn a_bounded_wait_gives_up_on_time_and_a_nested_call_waits_for_nothing() {
    let s = Slots::new(1, 3);
    let held = s.hold(1);
    let start = Instant::now();
    let out = s.run(&["--wait", "1", "--", "touch", "ran"]);
    let took = start.elapsed();
    assert_eq!(out.status.code(), Some(75));
    assert!(
        took >= Duration::from_secs(1) && took < Duration::from_millis(1800),
        "{took:?}"
    );
    assert!(text(&out.stderr).contains("heavy: all 1 slots busy, waiting"));
    assert!(text(&out.stderr).contains("every slot still busy after 1s"));
    let out = s.run(&["--wait", "0", "--", "touch", "ran"]);
    assert_eq!(out.status.code(), Some(75));
    assert!(!s.path().join("ran").exists());
    // Inside a slot, it runs at once, in heavy's place.
    let out = s
        .heavy(&["--wait", "0", "--", "sh", "-c", "echo $HEAVY_SLOT"])
        .env("HEAVY_SLOT", "1")
        .output()
        .unwrap();
    assert_eq!(
        (out.status.code(), text(&out.stdout)),
        (Some(0), "1\n".into())
    );
    // Unbounded, it gets the slot once it's free.
    let mut waiting = s
        .heavy(&["--", "touch", "ran"])
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    thread::sleep(Duration::from_millis(300));
    assert!(waiting.try_wait().unwrap().is_none());
    drop(held);
    assert!(exits(&mut waiting).success());
    assert!(s.path().join("ran").exists());
}

#[test]
fn all_takes_every_slot_and_all_the_threads() {
    let s = Slots::new(3, 3);
    let status = format!(
        "{} {}",
        burrow_test::program!("burrow-heavy"),
        s.config.display()
    );
    let out = s.run(&[
        "--all",
        "--",
        "sh",
        "-c",
        &format!("echo $HEAVY_SLOT $HEAVY_JOBS $CARGO_BUILD_JOBS $RUST_TEST_THREADS; {status} status; exit 7"),
    ]);
    assert_eq!(out.status.code(), Some(7));
    let all = threads();
    let share = (all / 3).max(1);
    assert_eq!(
        text(&out.stdout),
        format!(
            "all {all} {all} {all}\n3 slots of {share} threads (of {all})\nslot1: busy\nslot2: busy\nslot3: busy\n"
        )
    );
    assert_eq!(s.status().matches(": free").count(), 3, "every slot back");
}

#[test]
fn all_holds_what_it_has_while_it_waits_for_the_rest() {
    let s = Slots::new(3, 3);
    let held = s.hold(2);
    let mut waiting = s
        .heavy(&["--all", "--", "touch", "ran"])
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Slot 1 is taken and kept while slot 2 is busy; slot 3 isn't asked yet.
    until("slot1 taken", || s.status().contains("slot1: busy"));
    thread::sleep(Duration::from_millis(300));
    assert!(waiting.try_wait().unwrap().is_none());
    assert!(!s.path().join("ran").exists());
    assert!(s.status().contains("slot3: free"));
    drop(held);
    assert!(exits(&mut waiting).success());
    assert!(s.path().join("ran").exists());
    let err = text(&waiting.wait_with_output().unwrap().stderr);
    assert!(err.contains("heavy: waiting for all 3 slots"), "{err}");
}

#[test]
fn no_ordinary_run_starts_while_all_waits() {
    let s = Slots::new(2, 3);
    let held = s.hold(1);
    let mut waiting = s
        .heavy(&["--all", "--", "touch", "ran"])
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    until("the turn taken", || {
        let all = File::open(s.path().join("all")).unwrap();
        flock(&all, FlockOperation::NonBlockingLockShared).is_err()
    });
    // Slot 2 is free, but the waiting run has the turn: a new arrival
    // doesn't take it, so arrivals can't keep the machine from emptying.
    let out = s.run(&["--wait", "0", "--", "touch", "ordinary"]);
    assert_eq!(out.status.code(), Some(75));
    assert!(!s.path().join("ordinary").exists());
    drop(held);
    assert!(exits(&mut waiting).success());
    assert!(s.path().join("ran").exists());
    // Once it's done, ordinary runs start again.
    assert!(
        s.run(&["--wait", "0", "--", "touch", "ordinary"])
            .status
            .success()
    );
}

#[test]
fn without_the_turn_file_ordinary_runs_go_on_and_all_cant_run() {
    let s = Slots::new(1, 3);
    fs::remove_file(s.path().join("all")).unwrap();
    assert!(s.run(&["--", "touch", "ordinary"]).status.success());
    assert!(s.path().join("ordinary").exists());
    let out = s.run(&["--all", "--", "touch", "ran"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("cannot open"),
        "{}",
        text(&out.stderr)
    );
    assert!(!s.path().join("ran").exists());
}

#[test]
fn all_starts_nothing_once_its_wait_has_run_out() {
    let s = Slots::new(2, 3);
    let held = s.hold(2);
    let mut waiting = s
        .heavy(&["--wait", "1", "--all", "--", "touch", "ran"])
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Slot 2 comes free while it naps to its deadline; waking after it, the
    // run gives up instead of taking the slot.
    thread::sleep(Duration::from_millis(500));
    drop(held);
    assert_eq!(exits(&mut waiting).code(), Some(75));
    assert!(!s.path().join("ran").exists());
    assert_eq!(s.status().matches(": free").count(), 2, "every slot back");
}

#[test]
fn all_gives_back_what_it_took_when_its_wait_runs_out() {
    let s = Slots::new(2, 3);
    let _held = s.hold(2);
    let start = Instant::now();
    let out = s.run(&["--wait", "1", "--all", "--", "touch", "ran"]);
    let took = start.elapsed();
    assert_eq!(out.status.code(), Some(75));
    assert!(
        took >= Duration::from_secs(1) && took < Duration::from_millis(1800),
        "{took:?}"
    );
    assert!(text(&out.stderr).contains("slot2 still busy after 1s"));
    assert!(!s.path().join("ran").exists());
    assert!(s.status().contains("slot1: free"), "slot 1 given back");
    // Inside a slot it takes no other, so it can't wait on its own.
    let out = s
        .heavy(&["--all", "--", "sh", "-c", "echo $HEAVY_SLOT"])
        .env("HEAVY_SLOT", "1")
        .output()
        .unwrap();
    assert_eq!(
        (out.status.code(), text(&out.stdout)),
        (Some(0), "1\n".into())
    );
}

/// A heavy that waits, killed if it's still about when the test ends.
struct Waiter(Child);

impl Waiter {
    /// Starts heavy to note `name` in `order` once it runs, and returns once
    /// it says it waits: it's in line by then.
    fn start(s: &Slots, options: &[&str], name: &str) -> Waiter {
        let note = format!("echo {name} >>order");
        let mut child = s
            .heavy(&[options, &["--", "sh", "-c", &note]].concat())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut said = String::new();
        let mut stderr = BufReader::new(child.stderr.take().unwrap());
        stderr.read_line(&mut said).unwrap();
        let waiter = Waiter(child);
        assert!(said.contains("waiting"), "{name}: {said}");
        waiter
    }

    fn done(mut self) {
        assert!(exits(&mut self.0).success());
    }
}

impl Drop for Waiter {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn order(s: &Slots) -> String {
    fs::read_to_string(s.path().join("order")).unwrap_or_default()
}

#[test]
fn the_longest_waiter_takes_the_next_slot_freed() {
    let s = Slots::new(2, 3);
    let (_busy, freed) = (s.hold(1), s.hold(2));
    let mut waiters = Vec::new();
    let mut began = None;
    for name in ["a", "b", "c", "d"] {
        waiters.push(Waiter::start(&s, &[], name));
        began.get_or_insert_with(Instant::now);
        thread::sleep(Duration::from_millis(300));
    }
    // Just past the longest waiter's look (they look each second), so the
    // others' looks come first: unordered, one of those would get the slot.
    let began: Instant = began.unwrap();
    thread::sleep(Duration::from_millis(2100).saturating_sub(began.elapsed()));
    drop(freed);
    waiters.into_iter().for_each(Waiter::done);
    assert_eq!(order(&s), "a\nb\nc\nd\n");
}

#[test]
fn a_newcomer_never_takes_the_slot_a_waiter_is_in_line_for() {
    let s = Slots::new(1, 3);
    let held = s.hold(1);
    let waiter = Waiter::start(&s, &[], "waiter");
    drop(held);
    let out = s.run(&["--wait", "0", "--", "touch", "newcomer"]);
    assert_eq!(out.status.code(), Some(75));
    let err = text(&out.stderr);
    assert!(err.contains("others still ahead in line after 0s"), "{err}");
    assert!(!s.path().join("newcomer").exists());
    waiter.done();
    assert_eq!(order(&s), "waiter\n");
}

#[test]
fn first_goes_ahead_of_whoever_waits_and_in_order_among_its_own() {
    let s = Slots::new(1, 3);
    let held = s.hold(1);
    let ordinary = Waiter::start(&s, &[], "ordinary");
    let update = Waiter::start(&s, &["--first"], "update");
    let later = Waiter::start(&s, &["--first"], "later");
    drop(held);
    for waiter in [update, later, ordinary] {
        waiter.done();
    }
    assert_eq!(order(&s), "update\nlater\nordinary\n");
}

#[test]
fn all_goes_ahead_of_first_and_in_order_among_its_own() {
    let s = Slots::new(1, 3);
    let held = s.hold(1);
    let first = Waiter::start(&s, &["--first"], "first");
    let all = Waiter::start(&s, &["--all"], "all");
    let next = Waiter::start(&s, &["--all"], "next");
    drop(held);
    for waiter in [all, next, first] {
        waiter.done();
    }
    assert_eq!(order(&s), "all\nnext\nfirst\n");
}

#[test]
fn a_waiter_that_is_killed_or_gives_up_leaves_the_line() {
    let s = Slots::new(1, 3);
    let held = s.hold(1);
    let killed = Waiter::start(&s, &[], "killed");
    let mut gave_up = Waiter::start(&s, &["--wait", "1"], "gave-up");
    let next = Waiter::start(&s, &[], "next");
    assert_eq!(exits(&mut gave_up.0).code(), Some(75));
    drop(killed);
    drop(held);
    next.done();
    assert_eq!(order(&s), "next\n");
}

/// A cancellable run, whose worker writes its pid to `worker` once its
/// traps are set. Whatever is left of it when the test ends is killed.
struct WorkerRun {
    child: Child,
    file: PathBuf,
}

impl WorkerRun {
    fn spawn(s: &Slots, script: &str) -> WorkerRun {
        let child = s.heavy(&["--", "sh", "-c", script]).spawn().unwrap();
        WorkerRun {
            child,
            file: s.path().join("worker"),
        }
    }

    /// Spawns it and waits for its worker.
    fn start(s: &Slots, script: &str) -> WorkerRun {
        let run = WorkerRun::spawn(s, script);
        until("a worker", || run.read_worker().is_some());
        run
    }

    fn read_worker(&self) -> Option<i32> {
        let text = fs::read_to_string(&self.file).ok()?;
        text.strip_suffix('\n')?.parse().ok()
    }

    fn worker(&self) -> i32 {
        self.read_worker().unwrap()
    }
}

impl Drop for WorkerRun {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(worker) = self.read_worker().and_then(rustix::process::Pid::from_raw)
            && let Ok(group) = rustix::process::getpgid(Some(worker))
        {
            let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        }
    }
}

fn alive(pid: i32) -> bool {
    rustix::process::test_kill_process(rustix::process::Pid::from_raw(pid).unwrap()).is_ok()
}

fn signal(child: &Child, sig: rustix::process::Signal) {
    rustix::process::kill_process(rustix::process::Pid::from_child(child), sig).unwrap();
}

#[test]
fn a_cancelled_run_stops_all_of_it_before_the_slot_goes() {
    use rustix::process::Signal;
    for sig in [Signal::TERM, Signal::INT] {
        let s = Slots::new(1, 3);
        let mut run = WorkerRun::start(
            &s,
            r#"sh -c 'trap "sleep 1; touch stopped; exit" TERM; echo $$ >worker; while :; do sleep 0.1; done' & wait"#,
        );
        signal(&run.child, sig);
        exits(&mut run.child);
        assert!(
            s.path().join("stopped").exists(),
            "{sig:?}: the slot went before the worker stopped"
        );
        assert!(!alive(run.worker()), "{sig:?}: the worker outlived heavy");
        assert!(s.status().contains("slot1: free"), "{sig:?}");
    }
}

// Until setsid runs, the command is in heavy's own group: the signal waits
// for the command's group rather than miss it.
#[test]
fn a_run_cancelled_before_its_command_has_a_group_still_stops_it() {
    // A grace long enough that a lost signal can't pass for one that came.
    let s = Slots::new(1, 10);
    s.slow_setsid();
    let mut run = WorkerRun::spawn(&s, "echo $$ >worker; while :; do sleep 0.1; done");
    until("setsid to start", || s.path().join("started").exists());
    let start = Instant::now();
    signal(&run.child, rustix::process::Signal::TERM);
    let status = exits(&mut run.child);
    let took = start.elapsed();
    assert_eq!(
        status.code(),
        Some(128 + 15),
        "the command's own status, after {took:?}: killed after the grace is 137"
    );
}

#[test]
fn what_ignores_the_signal_is_killed_after_the_grace() {
    let s = Slots::new(1, 2);
    let mut run = WorkerRun::start(
        &s,
        r#"trap "" TERM INT HUP; echo $$ >worker; while :; do sleep 0.1; done"#,
    );
    let start = Instant::now();
    signal(&run.child, rustix::process::Signal::HUP);
    let status = exits(&mut run.child);
    let took = start.elapsed();
    assert!(
        took >= Duration::from_millis(1900) && took < Duration::from_secs(5),
        "{took:?}"
    );
    assert_eq!(
        status.code(),
        Some(128 + 9),
        "the command's own status: killed"
    );
    assert!(!alive(run.worker()));
    assert!(s.status().contains("slot1: free"));
}

#[test]
fn nix_builds_get_the_share_and_caller_settings_win() {
    let s = Slots::new(3, 3);
    for (args, share) in [
        (vec!["--"], (threads() / 3).max(1)),
        (vec!["--all", "--"], threads()),
    ] {
        let mut args = args;
        args.extend(["sh", "-c", "printf '%s' \"$NIX_CONFIG\""]);
        let defaults = format!("max-jobs = 1\ncores = {share}\n");
        let out = s.run(&args);
        assert!(out.status.success());
        assert_eq!(text(&out.stdout), defaults);
        let caller = "# caller\nmax-jobs = 2\ncores = 1\nsubstitute = false";
        let out = s.heavy(&args).env("NIX_CONFIG", caller).output().unwrap();
        assert!(out.status.success());
        // Nix reads assignments in order: the caller's later values win.
        assert_eq!(text(&out.stdout), format!("{defaults}{caller}"));
    }
}
