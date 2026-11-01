//! The binary as the heartbeat unit runs it: its config, the ping key as a
//! credential, the streaks in its runtime directory, a stand-in HOST-health
//! and a stand-in Healthchecks (common/mod.rs).

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

use common::Healthchecks;

/// The stand-in HOST-health, written once for every test: it says what
/// $HEALTH_REPORT holds and exits $HEALTH_EXIT. A file being written while
/// another test starts a process is open for writing in that process until
/// it execs, and running the file then fails with "Text file busy". No test
/// starts one before this is written, and none is written after.
fn health() -> &'static Path {
    static HEALTH: OnceLock<(tempfile::TempDir, PathBuf)> = OnceLock::new();
    let (_, path) = HEALTH.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("burrow-health");
        fs::write(
            &path,
            "#!/bin/sh\ncat \"$HEALTH_REPORT\"\nexit \"${HEALTH_EXIT:-0}\"\n",
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        (dir, path)
    });
    path
}

struct Unit {
    _dir: tempfile::TempDir,
    config: PathBuf,
    creds: PathBuf,
    run: PathBuf,
    report: PathBuf,
}

impl Unit {
    fn new(url: &str) -> Unit {
        let health = health();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let config = root.join("box-heartbeat.json");
        let json = serde_json::json!({
            "name": "box-heartbeat", "url": url, "check": "box", "failAfter": 2,
            "skip": ["github"], "health": health,
        });
        fs::write(&config, json.to_string()).unwrap();
        let (creds, run) = (root.join("creds"), root.join("run"));
        fs::create_dir(&creds).unwrap();
        fs::create_dir(&run).unwrap();
        fs::write(creds.join("ping-key"), " KEY \n").unwrap();
        Unit {
            report: root.join("report"),
            _dir: dir,
            config,
            creds,
            run,
        }
    }

    fn command(&self, lines: &[&str]) -> Command {
        fs::write(
            &self.report,
            lines.iter().map(|l| format!("{l}\n")).collect::<String>(),
        )
        .unwrap();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_burrow-heartbeat"));
        cmd.arg(&self.config)
            .env("CREDENTIALS_DIRECTORY", &self.creds)
            .env("RUNTIME_DIRECTORY", &self.run)
            .env("HEALTH_REPORT", &self.report);
        for var in [
            "ALL_PROXY",
            "all_proxy",
            "HTTPS_PROXY",
            "https_proxy",
            "HTTP_PROXY",
            "http_proxy",
        ] {
            cmd.env_remove(var);
        }
        cmd
    }

    fn beat(&self, lines: &[&str]) -> Output {
        self.command(lines).output().unwrap()
    }
}

fn said(out: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn a_beat_pings_its_check_with_the_key_its_credential_holds() {
    let hc = Healthchecks::serve("/KEY/box", &[]);
    let unit = Unit::new(&hc.url);
    let out = unit.beat(&["check:sshd ok", "check:github fail", "check:events ok"]);
    assert!(out.status.success(), "{:?}", said(&out));
    assert_eq!(
        said(&out),
        ("/ healthy: 2 checks pass\n".to_owned(), String::new())
    );
    assert_eq!(
        hc.last(),
        ("/KEY/box".to_owned(), "healthy: 2 checks pass\n".to_owned())
    );
}

#[test]
fn a_check_failing_two_beats_running_pings_fail() {
    let hc = Healthchecks::serve("/KEY/box", &[]);
    let unit = Unit::new(&hc.url);
    unit.beat(&["check:zfs fail"]);
    let out = unit.beat(&["check:zfs fail", "user-unit:music.service fail"]);
    assert_eq!(
        said(&out).0,
        "/fail failing for 2 beats or more: zfs\nfailed units (not counted): music.service\n"
    );
    assert_eq!(hc.last().0, "/KEY/box/fail");
    assert_eq!(fs::read_to_string(unit.run.join("zfs")).unwrap(), "2\n");
}

#[test]
fn a_failed_health_pings_the_log() {
    let hc = Healthchecks::serve("/KEY/box", &[]);
    let unit = Unit::new(&hc.url);
    let out = unit
        .command(&["check:zfs ok"])
        .env("HEALTH_EXIT", "3")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        said(&out).0,
        "/log burrow-health failed (exit 3), so this beat can't tell how the machine is\n"
    );
    assert_eq!(hc.last().0, "/KEY/box/log");
}

#[test]
fn a_ping_that_doesnt_land_is_said_and_fails_nothing() {
    let hc = Healthchecks::serve("/KEY/box", &[]);
    let unit = Unit::new(&hc.url);
    fs::write(unit.creds.join("ping-key"), "NOTKEY\n").unwrap();
    let out = unit.beat(&["check:sshd ok"]);
    assert!(out.status.success(), "a refused ping failed the beat");
    let (_, err) = said(&out);
    assert_eq!(
        err,
        "the ping to box didn't land (HTTP 404); Healthchecks alerts once the check's grace runs out\n"
    );
    let gone = Unit::new(&common::nobody());
    let out = gone.beat(&["check:sshd ok"]);
    assert!(out.status.success());
    let (_, err) = said(&out);
    assert!(err.starts_with("the ping to box didn't land ("), "{err}");
    assert!(!err.contains("KEY"), "{err}");
}

#[test]
fn outside_its_unit_it_says_so() {
    let unit = Unit::new(&common::nobody());
    for var in ["CREDENTIALS_DIRECTORY", "RUNTIME_DIRECTORY"] {
        let out = unit.command(&[]).env_remove(var).output().unwrap();
        assert_eq!(out.status.code(), Some(1), "{var}");
        assert_eq!(
            said(&out).1,
            format!("box-heartbeat: {var} is unset: run it as the box-heartbeat unit\n")
        );
    }
    fs::remove_file(unit.creds.join("ping-key")).unwrap();
    let out = unit.beat(&[]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        said(&out).1.starts_with("box-heartbeat: "),
        "{:?}",
        said(&out)
    );
}

#[test]
fn what_it_cant_run() {
    let unit = Unit::new(&common::nobody());
    let bin = Path::new(env!("CARGO_BIN_EXE_burrow-heartbeat"));
    assert_eq!(
        Command::new(bin).output().unwrap().status.code(),
        Some(2),
        "no config"
    );
    let extra = unit.config.with_file_name("extra.json");
    let mut json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&unit.config).unwrap()).unwrap();
    json["every"] = "5min".into();
    fs::write(&extra, json.to_string()).unwrap();
    let out = Command::new(bin).arg(&extra).output().unwrap();
    assert_eq!(out.status.code(), Some(2), "an unknown field");
    assert!(
        said(&out).1.contains("unknown field `every`"),
        "{:?}",
        said(&out)
    );
}
