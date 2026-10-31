//! The binary as `heavy` runs it, on slots of its own. The module's whole
//! tool, as it's wrapped, is held to the rest by flake.nix's `cpu` check.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
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

    fn run(&self, args: &[&str]) -> Output {
        self.heavy(args).output().unwrap()
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
    // A command that couldn't start, as a shell says it.
    assert_eq!(s.run(&["--", "./no-such-command"]).status.code(), Some(127));
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
    assert!(waiting.wait().unwrap().success());
    assert!(s.path().join("ran").exists());
}

/// Starts a cancellable run whose worker writes its pid to `worker` once
/// its traps are set.
fn worker_run(s: &Slots, script: &str) -> (Child, i32) {
    let child = s.heavy(&["--", "sh", "-c", script]).spawn().unwrap();
    let worker = s.path().join("worker");
    until("a worker", || {
        fs::read_to_string(&worker).is_ok_and(|w| w.ends_with('\n'))
    });
    let pid = fs::read_to_string(&worker).unwrap().trim().parse().unwrap();
    (child, pid)
}

fn alive(pid: i32) -> bool {
    rustix::process::test_kill_process(rustix::process::Pid::from_raw(pid).unwrap()).is_ok()
}

fn signal(child: &Child, sig: rustix::process::Signal) {
    rustix::process::kill_process(rustix::process::Pid::from_child(child), sig).unwrap();
}

#[test]
fn a_cancelled_run_stops_all_of_it_before_the_slot_goes() {
    let s = Slots::new(1, 3);
    let (mut child, worker) = worker_run(
        &s,
        r#"sh -c 'trap "sleep 1; touch stopped; exit" TERM; echo $$ >worker; while :; do sleep 0.1; done' & wait"#,
    );
    signal(&child, rustix::process::Signal::TERM);
    child.wait().unwrap();
    assert!(
        s.path().join("stopped").exists(),
        "the slot went before the worker stopped"
    );
    assert!(!alive(worker), "the worker outlived heavy");
    assert!(s.status().contains("slot1: free"));
}

#[test]
fn what_ignores_the_signal_is_killed_after_the_grace() {
    let s = Slots::new(1, 2);
    let (mut child, worker) = worker_run(
        &s,
        r#"trap "" TERM INT HUP; echo $$ >worker; while :; do sleep 0.1; done"#,
    );
    let start = Instant::now();
    signal(&child, rustix::process::Signal::HUP);
    let status = child.wait().unwrap();
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
    assert!(!alive(worker));
    assert!(s.status().contains("slot1: free"));
}
