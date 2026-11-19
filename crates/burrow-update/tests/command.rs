//! HOST-update as the machine runs it: burrow-update CONFIG [--force |
//! status], with stand-ins for what it runs as root that keep how they were
//! run in FAKE, a dir of the test's own. A run's own steps are the
//! library's tests (run_tests.rs); these are its words, what it says, and
//! its exits.

use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

use burrow_test::{root, said};

const BIN: &str = env!("CARGO_BIN_EXE_burrow-update");

/// Shared stand-ins, installed by burrow_test (sudo, systemctl, runuser,
/// box-notify, systemd-cat) and the test's own, cached for the test binary.
/// Each keeps what it's run with in FAKE, the test's.
fn bin() -> &'static Path {
    static BIN: OnceLock<tempfile::TempDir> = OnceLock::new();
    BIN.get_or_init(|| {
        let bin = burrow_test::stand_ins(&[
            "sudo",
            "systemctl",
            "runuser",
            "box-notify",
            "systemd-cat",
        ]);
        for (name, text) in [
            (
                "install",
                "#!/bin/sh\necho \"$*\" >>\"$FAKE/install\"\necho \"install: cannot change owner\" >&2\nexit 1\n",
            ),
        ]
        .into_iter()
        .chain(OTHERS.iter().map(|&name| {
            (
                name,
                "#!/bin/sh\necho \"${0##*/} $*\" >>\"$FAKE/others\"\n",
            )
        })) {
            let path = bin.path().join(name);
            burrow_test::executable(&path, text);
        }
        bin
    })
    .path()
}

/// What these tests never get as far as running: each only says it ran.
const OTHERS: [&str; 5] = ["box-switch", "box-confirm", "box-health", "vulnix", "curl"];

struct Machine {
    dir: tempfile::TempDir,
}

impl Machine {
    /// A machine with nothing run yet: no clone, no state.
    fn new() -> Machine {
        let m = Machine {
            dir: tempfile::tempdir().unwrap(),
        };
        fs::create_dir(m.path("fake")).unwrap();
        // What the systemctl stand-in lists: one timer.
        fs::write(
            m.path("fake/systemctl.says"),
            "printf 'NEXT LEFT UNIT\\nSun 10:00 5min burrow-events-catch-up.timer\\n\\n1 timers listed.\\n'\n",
        )
        .unwrap();
        if root() {
            // For nobody (`command`), who may write where it keeps its
            // state and the stand-ins keep what they did.
            for p in ["", "fake"] {
                burrow_test::permissions(m.path(p), 0o777);
            }
        }
        let b = bin();
        let config = serde_json::json!({
            "update": "box-update", "host": "box", "state": m.path("state"), "work": m.path("scratch/work"),
            "user": "sam", "home": m.path("home"), "uid": 1000, "runuser": b.join("runuser"), "path": "/b",
            "configuration": "box", "system": "x86_64-linux", "heavy": null,
            "project": "homelab", "app": null, "train": null,
            "origin": "https://github.com/sam/homelab", "flake": m.path("operator"),
            "group": "users", "owner": "Sam", "manifest": null,
            "releases": "https://downloads.example/claude", "checks": ["vm"],
            "within": "15min", "switchState": m.path("switch"), "currentSystem": m.path("current-system"),
            "lock": m.path("update.lock"), "timers": ["burrow-events-catch-up.timer"],
            "tools": {"sudo": b.join("sudo"), "switch": b.join("box-switch"),
                      "confirm": b.join("box-confirm"), "boot": null, "health": b.join("box-health"),
                      "notify": b.join("box-notify"), "vulnix": b.join("vulnix"), "curl": b.join("curl"),
                      "install": b.join("install"), "logger": b.join("systemd-cat"),
                      "systemctl": b.join("systemctl")},
        });
        fs::write(m.path("config.json"), config.to_string()).unwrap();
        m
    }

    fn path(&self, p: &str) -> PathBuf {
        self.dir.path().join(p)
    }

    /// HOST-update with `words`, as anyone but root starts it, so it goes
    /// through sudo wherever the tests run: as nobody, when they run as
    /// root.
    fn command(&self, words: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.arg(self.path("config.json"))
            .args(words)
            .env("FAKE", self.path("fake"));
        if root() {
            c.uid(65534).gid(65534);
        }
        c
    }

    fn run(&self, words: &[&str]) -> Output {
        self.command(words).output().unwrap()
    }

    /// How the stand-in `name` was run.
    fn fake(&self, name: &str) -> String {
        fs::read_to_string(self.path("fake").join(name)).unwrap_or_default()
    }
}

#[test]
fn a_usage_or_a_config_it_cant_take_exits_2_having_run_nothing() {
    let m = Machine::new();
    let out = Command::new(BIN).output().unwrap();
    assert_eq!(
        said(&out),
        (
            Some(2),
            String::new(),
            "usage: burrow-update CONFIG [--force | status]\n".into()
        )
    );
    let out = Command::new(BIN)
        .arg(m.path("none.json"))
        .arg("status")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(said(&out).2.starts_with("burrow-update: "));
    for words in [
        &["go"][..],
        &["status", "--force"],
        &["--force", "--force"],
        &["__through-sudo", "go"],
        &["-f"],
    ] {
        assert_eq!(
            said(&m.run(words)),
            (
                Some(2),
                String::new(),
                "usage: box-update [--force | status]\n".into()
            ),
            "{words:?}"
        );
    }
    assert_eq!(m.fake("sudo"), "");
    assert!(!m.path("state").exists());
    // The VM tests a run is told to run instead go into flake references
    // as well. The run reads them, once it's through sudo, which keeps none
    // of this environment: so start it as sudo starts it, with its word.
    let out = m
        .command(&["__through-sudo"])
        .env("BURROW_UPDATE_CHECKS", "vm ../x")
        .output()
        .unwrap();
    assert_eq!(
        said(&out),
        (
            Some(2),
            String::new(),
            "box-update: ../x: not a VM test's name\n".into()
        )
    );
    assert_eq!(m.fake("install"), "", "nothing run");
}

#[test]
fn status_goes_through_sudo_for_anyone_else() {
    let m = Machine::new();
    let out = m.run(&["status"]);
    assert_eq!(
        said(&out),
        (
            Some(0),
            "no run yet\nSun 10:00 5min burrow-events-catch-up.timer\n\n".into(),
            String::new()
        )
    );
    assert_eq!(
        m.fake("sudo"),
        format!(
            "-- {BIN} {} __through-sudo status\n",
            m.path("config.json").display()
        )
    );
    assert_eq!(
        m.fake("systemctl"),
        "list-timers burrow-events-catch-up.timer --no-pager\n"
    );
}

#[test]
fn a_run_says_what_it_did_and_why_it_broke() {
    let m = Machine::new();
    // A switch someone has pending: held, said and kept.
    fs::create_dir(m.path("switch")).unwrap();
    fs::write(m.path("switch/pending"), "").unwrap();
    for words in [&[][..], &["--force"]] {
        let out = m.run(words);
        assert_eq!(
            said(&out),
            (
                Some(0),
                "a box-switch is pending; trying again next run\n".into(),
                String::new()
            ),
            "{words:?}"
        );
        let status: serde_json::Value =
            serde_json::from_slice(&fs::read(m.path("state/status.json")).unwrap()).unwrap();
        assert_eq!(status["result"], "held");
    }
    assert!(m.path("state/jobs").is_dir() && m.path("state/logs").is_dir());
    // Its clone not made: it broke, said on stderr, and nothing kept.
    fs::remove_file(m.path("switch/pending")).unwrap();
    fs::remove_file(m.path("state/status.json")).unwrap();
    let out = m.run(&[]);
    assert_eq!(
        said(&out),
        (
            Some(1),
            String::new(),
            format!(
                "install: cannot change owner\nbox-update: {}: couldn't be made\n",
                m.path("scratch").display()
            )
        )
    );
    assert_eq!(
        m.fake("install"),
        format!("-d -o sam -g users {}\n", m.path("scratch").display())
    );
    assert!(!m.path("state/status.json").exists());
    assert_eq!(m.fake("others"), "", "nothing past the clone");
    // The stand-ins burrow_test keeps say what they were run with in their
    // own files.
    for kept in ["runuser", "notify", "journal"] {
        assert_eq!(m.fake(kept), "", "nothing past the clone: {kept}");
    }
}
