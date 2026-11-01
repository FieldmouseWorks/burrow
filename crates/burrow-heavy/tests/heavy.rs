//! The binary as `heavy` runs it, on slots of its own. The module's whole
//! tool, as it's wrapped, is held to the rest by flake.nix's `cpu` check.

use std::fs::{self, File};
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
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
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_burrow-heavy"));
        cmd.arg(&self.config)
            .args(args)
            .current_dir(self.path())
            .env_remove("HEAVY_SLOT")
            .env_remove("CARGO_BUILD_JOBS")
            .env_remove("RUST_TEST_THREADS");
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
        fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\ntouch started\nsleep 0.5\nexec {} \"$@\"\n",
                which("setsid").display()
            ),
        )
        .unwrap();
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
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
