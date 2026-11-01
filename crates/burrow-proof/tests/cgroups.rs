//! The fences for real, in a cgroup v2 directory this test may divide:
//! BURROW_PROOF_CGROUP names it (a root shell's `/sys/fs/cgroup/x`, made
//! for it). They're ignored by default, since a build sandbox has no such
//! thing; tests/proof-run.nix runs the same fences in a real unit.
//!
//!   mkdir /sys/fs/cgroup/proof-test
//!   BURROW_PROOF_CGROUP=/sys/fs/cgroup/proof-test cargo test -p burrow-proof --test cgroups -- --ignored

mod world;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use burrow_proof::fence::{Cgroups, clear_all, populated, unified};
use burrow_proof::run::run;
use world::{World, read};

const EXE: &str = env!("CARGO_BIN_EXE_burrow-proof");

/// A cgroup of this test's own, NAME, under BURROW_PROOF_CGROUP.
fn cgroup(name: &str) -> PathBuf {
    let root = std::env::var("BURROW_PROOF_CGROUP")
        .expect("BURROW_PROOF_CGROUP names a cgroup v2 directory");
    let dir = Path::new(&root).join(name);
    if dir.exists() {
        clear_all(&dir).unwrap();
        fs::remove_dir(&dir).unwrap();
    }
    fs::create_dir(&dir).unwrap();
    dir
}

/// Whether PID has ended (a zombie nobody reaped yet counts).
fn ended(pid: u32) -> bool {
    match fs::read_to_string(format!("/proc/{pid}/status")) {
        Ok(s) => s.contains("\nState:\tZ"),
        Err(_) => true,
    }
}

fn pid(path: &Path) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(p) = read(path).trim().parse() {
            return p;
        }
        assert!(Instant::now() < deadline, "no pid in {}", path.display());
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
#[ignore = "needs a cgroup v2 directory it may divide (BURROW_PROOF_CGROUP)"]
fn enter_runs_the_program_inside_its_fence() {
    let dir = cgroup("enter");
    let fence = dir.join("check-0");
    fs::create_dir(&fence).unwrap();
    let out = Command::new(EXE)
        .args([
            "enter",
            fence.to_str().unwrap(),
            "--",
            "cat",
            "/proc/self/cgroup",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let seen = unified(std::str::from_utf8(&out.stdout).unwrap())
        .unwrap()
        .to_owned();
    assert!(
        fence.ends_with(seen.trim_start_matches('/')),
        "{seen} vs {}",
        fence.display()
    );
    // One it can't enter runs nothing, and says so as a shell would.
    let out = Command::new(EXE)
        .args([
            "enter",
            "/no/such/cgroup",
            "--",
            "touch",
            dir.join("ran").to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(126));
    assert!(!dir.join("ran").exists());
    fs::remove_dir(&fence).unwrap();
}

#[test]
#[ignore = "needs a cgroup v2 directory it may divide (BURROW_PROOF_CGROUP)"]
fn what_a_command_leaves_is_ended_before_its_worktree_goes() {
    let w = World::new();
    let dir_cg = cgroup("run");
    let out = w.path().join("out");
    fs::create_dir(&out).unwrap();
    let o = out.display();
    // Two that outlive it: one ignoring SIGTERM, one in a session of its
    // own with its descriptors closed, both in the worktree.
    let cmd = format!(
        "(trap '' TERM; exec sleep 300) & echo $! >{o}/a; \
         setsid sh -c 'exec </dev/null >/dev/null 2>&1; trap \"\" TERM HUP; echo $$ >{o}/b; exec sleep 300' & \
         sleep 0.2; exit 0"
    );
    let dir = w.made("proof-sam-r-5-1", &w.record(&[("gate", &cmd)]));
    let mut fences = Cgroups {
        dir: dir_cg.clone(),
        exe: EXE.into(),
    };
    run(&dir, &mut fences).unwrap();
    let (a, b) = (pid(&out.join("a")), pid(&out.join("b")));
    assert!(ended(a) && ended(b), "{a} {b} outlived the run");
    assert!(!dir_cg.join("check-0").exists());
    assert!(w.trees_left().is_empty());
    assert_eq!(read(&dir.join("cgroup")), dir_cg.to_str().unwrap());
    assert!(read(&dir.join("ran.json")).contains("Ran here and passed"));
    fs::remove_dir(&dir_cg).unwrap();
}

#[test]
#[ignore = "needs a cgroup v2 directory it may divide (BURROW_PROOF_CGROUP)"]
fn clear_all_ends_every_fence_left() {
    let dir = cgroup("left");
    let mut kids = Vec::new();
    for i in 0..2 {
        let fence = dir.join(format!("check-{i}"));
        fs::create_dir(&fence).unwrap();
        kids.push(
            Command::new(EXE)
                .args([
                    "enter",
                    fence.to_str().unwrap(),
                    "--",
                    "sh",
                    "-c",
                    "trap '' TERM; sleep 300",
                ])
                .spawn()
                .unwrap(),
        );
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while !(populated(&dir.join("check-0")).unwrap() && populated(&dir.join("check-1")).unwrap()) {
        assert!(Instant::now() < deadline, "never entered");
        thread::sleep(Duration::from_millis(20));
    }
    // Reaped as they end, as a unit's manager would.
    let reaper = thread::spawn(move || {
        kids.into_iter()
            .map(|mut k| k.wait().unwrap())
            .collect::<Vec<_>>()
    });
    clear_all(&dir).unwrap();
    assert!(!dir.join("check-0").exists() && !dir.join("check-1").exists());
    assert!(reaper.join().unwrap().iter().all(|s| !s.success()));
    fs::remove_dir(&dir).unwrap();
}
