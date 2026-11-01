//! HOST-health as its callers run it: the config file, its checks in their
//! directory, and systemctl (here a stand-in that keeps how it was run and
//! says what FAILED and USER_FAILED hold).

use std::fs;
use std::io::Write;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

fn script(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// The stand-ins, written once for every test: a file being written while
/// another test starts a process is open for writing in that process until
/// it execs, and running the file then fails with "Text file busy". No test
/// starts one before these are written, and none is written after; a test's
/// checks are links to them.
fn stand_ins() -> &'static Path {
    static BIN: OnceLock<tempfile::TempDir> = OnceLock::new();
    BIN.get_or_init(|| {
        let bin = tempfile::tempdir().unwrap();
        let b = bin.path();
        script(
            &b.join("systemctl"),
            r#"#!/bin/sh
echo "$*" >>"$SYSTEMCTL_LOG"
case "$*" in
  --user*) printf '%s' "$USER_FAILED"; echo user-noise >&2; exit "${USER_EXIT:-0}" ;;
  *) printf '%s' "$FAILED"; echo system-noise >&2; exit "${SYSTEM_EXIT:-0}" ;;
esac
"#,
        );
        script(&b.join("pass"), "#!/bin/sh\nexit 0\n");
        script(&b.join("fail"), "#!/bin/sh\nexit 1\n");
        script(&b.join("noisy"), "#!/bin/sh\necho out\necho err >&2\n");
        // Passes only when it's given what HOST-health was.
        script(
            &b.join("stdin"),
            "#!/bin/sh\nread -r line && [ \"$line\" = hello ]\n",
        );
        // Fails twice, then passes.
        script(
            &b.join("counted"),
            "#!/bin/sh\nn=$(cat \"$COUNT\" 2>/dev/null || echo 0)\necho $((n + 1)) >\"$COUNT\"\n[ \"$n\" -ge 2 ]\n",
        );
        // Run out of time, with a child of their own: one dies of its TERM,
        // one exits 0 on it, and one ignores it.
        script(
            &b.join("slow"),
            "#!/bin/sh\nsleep 30 &\necho $! >>\"$PIDS\"\nwait\n",
        );
        script(
            &b.join("polite"),
            "#!/bin/sh\ntrap 'exit 0' TERM\nsleep 30 &\necho $! >>\"$PIDS\"\nwait\n",
        );
        script(
            &b.join("stubborn"),
            "#!/bin/sh\ntrap '' TERM\nsleep 30 &\necho $! >>\"$PIDS\"\nwait\nwait\n",
        );
        bin
    })
    .path()
}

struct Machine {
    dir: tempfile::TempDir,
}

impl Machine {
    /// A machine whose checks are these stand-ins, by these names.
    fn new(checks: &[(&str, &str)]) -> Machine {
        Machine::with(checks, &stand_ins().join("systemctl"))
    }

    fn with(checks: &[(&str, &str)], systemctl: &Path) -> Machine {
        // Before anything here can start a process.
        stand_ins();
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        fs::create_dir(d.join("health.d")).unwrap();
        for (name, stand_in) in checks {
            symlink(stand_ins().join(stand_in), d.join("health.d").join(name)).unwrap();
        }
        let config = serde_json::json!({
            "name": "box-health",
            "dir": d.join("health.d"),
            "user": "sam",
            "systemctl": systemctl,
            "checkTimeout": 1,
            "settle": 0,
        });
        fs::write(d.join("config.json"), config.to_string()).unwrap();
        Machine { dir }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.path(name)).unwrap_or_default()
    }

    fn run(&self, env: &[(&str, &str)], args: &[&str]) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_burrow-health"))
            .arg(self.path("config.json"))
            .args(args)
            .env("SYSTEMCTL_LOG", self.path("systemctl.log"))
            .env("COUNT", self.path("count"))
            .env("PIDS", self.path("pids"))
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // What a check that reads its stdin would get, if it got this.
        let _ = child.stdin.take().unwrap().write_all(b"hello\n");
        let deadline = Instant::now() + Duration::from_secs(30);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() > deadline {
                child.kill().unwrap();
                panic!("still running after 30 s: {args:?}");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        child.wait_with_output().unwrap()
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

const FAILED: &str = "a.service loaded failed failed The A\nsystemd-fsck@dev-disk-by\\x2duuid-1.service loaded failed failed File System Check\n";
const USER_FAILED: &str = "c.service loaded failed failed The C\n";
const SYSTEMCTL: &str = "list-units --failed --plain --no-legend --all\n--user -M sam@ list-units --failed --plain --no-legend --all\n";

#[test]
fn a_report_is_every_check_in_name_order_then_the_failed_units() {
    let m = Machine::new(&[
        ("noisy", "noisy"),
        ("github", "fail"),
        ("dns", "pass"),
        ("stdin", "stdin"),
        (".hidden", "fail"),
        ("broken", "missing"),
    ]);
    // A file that isn't executable isn't a check; a directory is one that
    // can't run.
    fs::write(m.path("health.d/readme"), "not a check").unwrap();
    fs::create_dir(m.path("health.d/subdir")).unwrap();
    let ran = m.run(&[("FAILED", FAILED), ("USER_FAILED", USER_FAILED)], &[]);
    assert_eq!(ran.status.code(), Some(0), "{ran:?}");
    assert_eq!(
        stdout(&ran),
        "check:dns ok\ncheck:github fail\ncheck:noisy ok\ncheck:stdin fail\ncheck:subdir fail\nunit:a.service fail\nunit:systemd-fsck@dev-disk-by\\x2duuid-1.service fail\nuser-unit:c.service fail\n"
    );
    assert_eq!(m.read("systemctl.log"), SYSTEMCTL);
    // The system's complaints are shown, the user's manager's aren't, and
    // the checks' own output goes nowhere.
    assert_eq!(stderr(&ran), "system-noise\n");
}

#[test]
fn what_systemctl_says_counts_even_when_it_fails() {
    let m = Machine::new(&[]);
    let ran = m.run(
        &[
            ("FAILED", FAILED),
            ("USER_FAILED", USER_FAILED),
            ("SYSTEM_EXIT", "1"),
            ("USER_EXIT", "1"),
        ],
        &[],
    );
    assert_eq!(ran.status.code(), Some(0), "{ran:?}");
    assert_eq!(
        stdout(&ran),
        "unit:a.service fail\nunit:systemd-fsck@dev-disk-by\\x2duuid-1.service fail\nuser-unit:c.service fail\n"
    );
    // No checks and nothing failed: an empty report.
    fs::remove_dir(m.path("health.d")).unwrap();
    let ran = m.run(&[], &[]);
    assert_eq!((ran.status.code(), stdout(&ran)), (Some(0), String::new()));
    // A systemctl that can't run says so, and the checks still count.
    let m = Machine::with(&[("dns", "pass")], Path::new("/nonexistent/systemctl"));
    let ran = m.run(&[], &[]);
    assert_eq!(
        (ran.status.code(), stdout(&ran)),
        (Some(0), "check:dns ok\n".to_owned())
    );
    assert!(
        stderr(&ran).starts_with("box-health: /nonexistent/systemctl: No such file"),
        "{ran:?}"
    );
}

/// Whether the process is gone: not there, or a zombie nobody has reaped
/// yet.
fn gone(pid: &str) -> bool {
    fs::read_to_string(format!("/proc/{pid}/stat"))
        .map(|stat| {
            stat.rsplit_once(") ")
                .is_some_and(|(_, rest)| rest.starts_with('Z'))
        })
        .unwrap_or(true)
}

#[test]
fn a_check_that_runs_out_of_time_fails_and_leaves_nothing_behind() {
    let m = Machine::new(&[("slow", "slow"), ("polite", "polite")]);
    let start = Instant::now();
    let ran = m.run(&[], &[]);
    let took = start.elapsed();
    // Out of time is a failure, whatever it exits with then.
    assert_eq!(
        (ran.status.code(), stdout(&ran)),
        (Some(0), "check:polite fail\ncheck:slow fail\n".to_owned())
    );
    // A second each: not their 30 s sleeps, nor the grace a check that
    // ignores its TERM gets.
    assert!(took < Duration::from_secs(5), "{took:?}");
    let pids = m.read("pids");
    assert_eq!(pids.lines().count(), 2);
    for pid in pids.lines() {
        assert!(gone(pid), "a check's own child is still running");
    }

    // One that ignores its TERM is killed once its grace is up.
    let m = Machine::new(&[("stubborn", "stubborn"), ("dns", "pass")]);
    let start = Instant::now();
    let ran = m.run(&[], &[]);
    let took = start.elapsed();
    assert_eq!(
        (ran.status.code(), stdout(&ran)),
        (Some(0), "check:dns ok\ncheck:stubborn fail\n".to_owned())
    );
    assert!(
        took >= Duration::from_secs(6) && took < Duration::from_secs(15),
        "{took:?}"
    );
    let pid = m.read("pids");
    assert!(gone(pid.trim()), "the check's own child is still running");
}

#[test]
fn output_it_cant_write_fails_it() {
    let m = Machine::new(&[("dns", "pass")]);
    let full = || {
        fs::OpenOptions::new()
            .write(true)
            .open("/dev/full")
            .unwrap()
    };
    fs::write(m.path("before"), "check:dns ok\n").unwrap();
    for args in [
        &[][..],
        &["compare", m.path("before").to_str().unwrap(), "0"],
    ] {
        let ran = Command::new(env!("CARGO_BIN_EXE_burrow-health"))
            .arg(m.path("config.json"))
            .args(args)
            .env("SYSTEMCTL_LOG", m.path("systemctl.log"))
            .stdout(full())
            .output()
            .unwrap();
        assert_eq!(ran.status.code(), Some(1), "{args:?}");
        assert!(
            stderr(&ran).ends_with("box-health: stdout: No space left on device (os error 28)\n"),
            "{ran:?}"
        );
    }
}

#[test]
fn a_snapshot_is_the_report() {
    let m = Machine::new(&[("dns", "pass"), ("github", "fail")]);
    let env = [("FAILED", FAILED), ("USER_FAILED", USER_FAILED)];
    let snap = m.path("before");
    let ran = m.run(&env, &["snapshot", snap.to_str().unwrap()]);
    assert_eq!(ran.status.code(), Some(0), "{ran:?}");
    assert_eq!(stdout(&ran), "");
    assert_eq!(m.read("before"), stdout(&m.run(&env, &[])));
    assert!(
        m.read("before")
            .starts_with("check:dns ok\ncheck:github fail\nunit:a.service fail\n")
    );
    // One it can't write.
    let ran = m.run(&env, &["snapshot", m.path("no/before").to_str().unwrap()]);
    assert_eq!(ran.status.code(), Some(1));
    assert!(
        stderr(&ran).contains(&format!(
            "box-health: {}: No such file",
            m.path("no/before").display()
        )),
        "{ran:?}"
    );
}

#[test]
fn what_was_already_broken_never_fails_a_compare() {
    let m = Machine::new(&[("dns", "pass"), ("github", "fail")]);
    let env = [("FAILED", FAILED), ("USER_FAILED", USER_FAILED)];
    let snap = m.path("before");
    assert!(
        m.run(&env, &["snapshot", snap.to_str().unwrap()])
            .status
            .success()
    );
    // The same: a failing check, and units already failed, one of them with
    // an escape in its name.
    let ran = m.run(&env, &["compare", snap.to_str().unwrap(), "0"]);
    assert_eq!(
        (ran.status.code(), stdout(&ran)),
        (Some(0), "healthy\n".to_owned()),
        "{ran:?}"
    );
    // Better than before: also healthy.
    let ran = m.run(&[], &["compare", snap.to_str().unwrap(), "0"]);
    assert_eq!(
        (ran.status.code(), stdout(&ran)),
        (Some(0), "healthy\n".to_owned())
    );
}

#[test]
fn a_compare_says_what_regressed() {
    let m = Machine::new(&[("dns", "fail"), ("github", "pass"), ("new", "fail")]);
    fs::write(
        m.path("before"),
        "check:dns ok\ncheck:github ok\nunit:a.service fail\n",
    )
    .unwrap();
    let ran = m.run(
        &[("FAILED", FAILED), ("USER_FAILED", USER_FAILED)],
        &["compare", m.path("before").to_str().unwrap(), "0"],
    );
    assert_eq!(ran.status.code(), Some(1), "{ran:?}");
    assert_eq!(
        stdout(&ran),
        "check:dns is failing\ncheck:new (new) is failing\nsystemd-fsck@dev-disk-by\\x2duuid-1.service failed\nc.service failed\n"
    );
}

#[test]
fn a_compare_waits_for_what_regressed_to_pass_again() {
    let m = Machine::new(&[("counted", "counted")]);
    fs::write(m.path("before"), "check:counted ok\n").unwrap();
    let before = m.path("before");
    let ran = m.run(&[], &["compare", before.to_str().unwrap(), "20"]);
    assert_eq!(
        (ran.status.code(), stdout(&ran)),
        (Some(0), "healthy\n".to_owned()),
        "{ran:?}"
    );
    assert_eq!(m.read("count"), "3\n");
    // Without SECONDS, it has its default to do it in.
    fs::remove_file(m.path("count")).unwrap();
    let ran = m.run(&[], &["compare", before.to_str().unwrap()]);
    assert_eq!(
        (ran.status.code(), stdout(&ran)),
        (Some(0), "healthy\n".to_owned())
    );
    // Each look waits for things to settle first.
    let mut config: serde_json::Value = serde_json::from_str(&m.read("config.json")).unwrap();
    config["settle"] = 1.into();
    fs::write(m.path("config.json"), config.to_string()).unwrap();
    fs::remove_file(m.path("count")).unwrap();
    let start = Instant::now();
    let ran = m.run(&[], &["compare", before.to_str().unwrap(), "0"]);
    assert!(start.elapsed() >= Duration::from_secs(1));
    assert_eq!(ran.status.code(), Some(1));
    config["settle"] = 0.into();
    fs::write(m.path("config.json"), config.to_string()).unwrap();
    // With no time, one look.
    fs::remove_file(m.path("count")).unwrap();
    let ran = m.run(&[], &["compare", before.to_str().unwrap(), "0"]);
    assert_eq!(
        (ran.status.code(), stdout(&ran)),
        (Some(1), "check:counted is failing\n".to_owned())
    );
    assert_eq!(m.read("count"), "1\n");
}

#[test]
fn a_snapshot_it_cant_read_fails_the_compare() {
    let m = Machine::new(&[("dns", "pass")]);
    let missing = m.path("missing");
    let ran = m.run(&[], &["compare", missing.to_str().unwrap(), "0"]);
    assert_eq!(ran.status.code(), Some(1), "{ran:?}");
    assert_eq!(
        stdout(&ran),
        format!(
            "the snapshot {} can't be read (No such file or directory (os error 2)), so nothing can be compared with it\n",
            missing.display()
        )
    );
    assert_eq!(m.read("systemctl.log"), "");
}

#[test]
fn what_it_cant_run() {
    let m = Machine::new(&[("dns", "pass")]);
    for args in [
        &["shout"][..],
        &["snapshot"],
        &["snapshot", ""],
        &["snapshot", "a", "b"],
        &["compare"],
        &["compare", ""],
        &["compare", "f", "soon"],
        &["compare", "f", "-1"],
        &["compare", "f", "+1"],
        &["compare", "f", ""],
        &["compare", "f", "1", "2"],
    ] {
        let ran = m.run(&[], args);
        assert_eq!(ran.status.code(), Some(2), "{args:?}");
        assert_eq!(
            stderr(&ran),
            "usage: box-health [snapshot FILE | compare FILE [SECONDS]]\n",
            "{args:?}"
        );
        assert_eq!(stdout(&ran), "", "{args:?}");
    }
    assert_eq!(m.read("systemctl.log"), "");
    let ran = Command::new(env!("CARGO_BIN_EXE_burrow-health"))
        .output()
        .unwrap();
    assert_eq!(ran.status.code(), Some(2));
    assert!(stderr(&ran).starts_with("usage: burrow-health CONFIG"));
    // A config it can't read, or with a field it doesn't know.
    let ran = Command::new(env!("CARGO_BIN_EXE_burrow-health"))
        .arg(m.path("missing.json"))
        .output()
        .unwrap();
    assert_eq!(ran.status.code(), Some(2));
    let mut config: serde_json::Value = serde_json::from_str(&m.read("config.json")).unwrap();
    config["timeout"] = 30.into();
    fs::write(m.path("config.json"), config.to_string()).unwrap();
    let ran = m.run(&[], &[]);
    assert_eq!(ran.status.code(), Some(2));
    assert!(stderr(&ran).contains("unknown field `timeout`"), "{ran:?}");
    assert_eq!(m.read("systemctl.log"), "");
}
