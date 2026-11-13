//! A clone leaves no descendant behind, even one in a session of its own.
//! Its own test binary: the checkout makes the process a subreaper.
use burrow_upkeep::checkout::{Stop, clone_with};
use rustix::time::{ClockId, clock_gettime};
use std::fs;
use std::path::Path;
use std::time::Instant;

fn deadline(secs: f64) -> f64 {
    let t = clock_gettime(ClockId::Monotonic);
    t.tv_sec as f64 + t.tv_nsec as f64 / 1e9 + secs
}

fn alive(pid: &str) -> bool {
    // A reaped process has no /proc entry; an unreaped zombie would.
    Path::new(&format!("/proc/{pid}")).exists()
}

fn fake(dir: &Path, body: &str) -> String {
    let path = dir.join("gh");
    burrow_test::executable(&path, format!("#!/bin/sh\n{body}\n"));
    path.display().to_string()
}

#[test]
fn detached_descendants_are_killed_and_reaped() {
    let tmp = tempfile::tempdir().unwrap();
    let pidfile = tmp.path().join("pid");
    let dest = tmp.path().join("projects/app");
    // Success, failure and timeout each leave a setsid sleeper behind.
    let cases = [
        ("mkdir -p \"$4\"; ", 30.0, true),
        ("mkdir -p \"$4\"; exit 1", 30.0, false),
        ("sleep 60", 1.0, false),
    ];
    for (i, (tail, secs, ok)) in cases.iter().enumerate() {
        let _ = fs::remove_file(&pidfile);
        let body = format!(
            "setsid sh -c 'echo $$ > {}; exec sleep 300' </dev/null >/dev/null 2>&1 &\n\
             while [ ! -s {} ]; do sleep 0.01; done\n{tail}",
            pidfile.display(),
            pidfile.display()
        );
        let program = fake(tmp.path(), &body);
        let started = Instant::now();
        let dest = dest.with_file_name(format!("app{i}"));
        let got = clone_with(&program, "o/app", &dest, deadline(*secs));
        let pid = fs::read_to_string(&pidfile).unwrap().trim().to_string();
        assert!(!alive(&pid), "case {i}: the detached sleeper survived");
        // Reaped within the bounded cleanup, not after the sleeper's 300s.
        assert!(started.elapsed().as_secs_f64() < secs + 8.0, "case {i}");
        assert_eq!(got.is_ok(), *ok, "case {i}");
        if *ok {
            assert!(dest.exists());
        } else {
            assert!(!dest.exists(), "case {i}: a failed clone was published");
        }
        if i == 2 {
            assert!(matches!(got, Err(Stop::Later(_))));
        }
    }
}
