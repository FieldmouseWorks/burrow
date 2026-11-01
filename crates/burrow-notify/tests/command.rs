//! HOST-notify as its callers run it: the config file, and on PATH gh, id,
//! runuser, chown and systemd-cat (here stand-ins that keep what they were
//! asked).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

fn script(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// A gh that answers `gh api -i` for sam/homelab with no issues yet, or
/// whose login GitHub refuses.
const GH: &str = r#"#!/bin/sh
echo "$*" >>"$LOG"
cat >/dev/null
[ -z "$DOWN" ] || { echo "HTTP 401: Bad credentials" >&2; exit 1; }
printf 'HTTP/2.0 '
case "$*" in
  *" GET repos/sam/homelab/issues?"*) printf '200 OK\n\n[]' ;;
  *" POST repos/sam/homelab/labels --input -") printf '201 Created\n\n{}' ;;
  *" POST repos/sam/homelab/issues --input -") printf '201 Created\n\n{"number":1,"labels":[{"name":"updates"}]}' ;;
  *) printf '404 Not Found\n\n{"message":"Not Found"}' ;;
esac
"#;

/// The stand-ins on PATH, written once for every test: each keeps what it
/// was asked in the file its variable names. A file being written while
/// another test starts a process is open for writing in that process until
/// it execs, and running the file then fails with "Text file busy". No test
/// starts one before these are written, and none is written after.
fn stand_ins() -> &'static Path {
    static BIN: OnceLock<tempfile::TempDir> = OnceLock::new();
    BIN.get_or_init(|| {
        let bin = tempfile::tempdir().unwrap();
        let b = bin.path();
        script(&b.join("gh"), GH);
        script(&b.join("id"), "#!/bin/sh\n[ \"$1\" = -un ] && echo \"$WHO\"\n");
        // runuser keeps what it was asked and runs the rest as the user.
        script(
            &b.join("runuser"),
            "#!/bin/sh\necho \"$*\" >>\"$RUNUSER\"\nwhile [ \"$1\" != -- ]; do shift; done\nshift\n[ -n \"$STAYS\" ] || WHO=sam\nWHO=$WHO exec \"$@\"\n",
        );
        script(&b.join("chown"), "#!/bin/sh\necho \"$*\" >>\"$CHOWN\"\n");
        script(
            &b.join("systemd-cat"),
            "#!/bin/sh\nprintf '%s|%s\\n' \"$*\" \"$(cat)\" >>\"$JOURNAL\"\n",
        );
        bin
    })
    .path()
}

struct Machine {
    dir: tempfile::TempDir,
}

impl Machine {
    fn new(repo: Option<&str>) -> Machine {
        // Before anything here can start a process.
        stand_ins();
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        fs::create_dir(d.join("notices")).unwrap();
        let config = serde_json::json!({
            "name": "box-notify",
            "repo": repo,
            "assignee": null,
            "label": "updates",
            "dir": d.join("notices"),
            "user": "sam",
            "home": d,
        });
        fs::write(d.join("config.json"), config.to_string()).unwrap();
        Machine { dir }
    }

    fn path(&self, name: &str) -> std::path::PathBuf {
        self.dir.path().join(name)
    }

    fn run(&self, who: &str, down: bool, args: &[&str]) -> Output {
        self.run_with(who, down, &[], args)
    }

    fn run_with(&self, who: &str, down: bool, env: &[(&str, &str)], args: &[&str]) -> Output {
        let path = format!(
            "{}:{}",
            stand_ins().display(),
            std::env::var("PATH").unwrap()
        );
        let mut child = Command::new(env!("CARGO_BIN_EXE_burrow-notify"))
            .arg(self.path("config.json"))
            .args(args)
            .env("PATH", path)
            .env("WHO", who)
            .env("DOWN", if down { "1" } else { "" })
            .env("LOG", self.path("gh.log"))
            .env("RUNUSER", self.path("runuser.log"))
            .env("CHOWN", self.path("chown.log"))
            .env("JOURNAL", self.path("journal"))
            .envs(env.iter().copied())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // One that runs itself again and again never ends: it fails here.
        let deadline = Instant::now() + Duration::from_secs(10);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() > deadline {
                child.kill().unwrap();
                panic!("still running after 10 s: {args:?}");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        child.wait_with_output().unwrap()
    }

    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.path(name)).unwrap_or_default()
    }
}

#[test]
fn a_notice_opens_is_pending_and_closes() {
    let m = Machine::new(Some("sam/homelab"));
    let ran = m.run("sam", false, &["open", "k1", "Stuck", "It's stuck."]);
    assert!(ran.status.success(), "{ran:?}");
    assert_eq!(
        m.read("gh.log").lines().collect::<Vec<_>>(),
        [
            "api -i -X GET repos/sam/homelab/issues?state=open&per_page=100",
            "api -i -X POST repos/sam/homelab/labels --input -",
            "api -i -X POST repos/sam/homelab/issues --input -",
        ],
        "journal: {}",
        m.read("journal")
    );
    assert_eq!(m.read("runuser.log"), "");
    assert_eq!(
        m.run("sam", false, &["pending", "k1"]).status.code(),
        Some(0)
    );
    assert_eq!(
        m.run("sam", false, &["pending", "k2"]).status.code(),
        Some(1)
    );
    let ran = m.run("sam", false, &["close", "k1", "Fixed."]);
    assert!(ran.status.success(), "{ran:?}");
    assert_eq!(
        m.run("sam", false, &["pending", "k1"]).status.code(),
        Some(1)
    );
    assert_eq!(m.read("journal"), "");
}

#[test]
fn anyone_else_runs_it_as_the_user() {
    let m = Machine::new(Some("sam/homelab"));
    let ran = m.run("root", false, &["open", "k1", "Stuck", "It's stuck."]);
    assert!(ran.status.success(), "{ran:?}");
    let d = m.dir.path();
    assert_eq!(
        m.read("runuser.log"),
        format!(
            "-u sam -- env HOME={} BURROW_NOTIFY_AS_USER=1 {} {} open k1 Stuck It's stuck.\n",
            d.display(),
            env!("CARGO_BIN_EXE_burrow-notify"),
            m.path("config.json").display()
        )
    );
    assert_eq!(
        m.read("chown.log"),
        format!("sam: {}\n", m.path("notices").display())
    );
    assert_eq!(m.read("gh.log").lines().count(), 3);
    // Asking whether one's pending needs neither.
    fs::remove_file(m.path("runuser.log")).unwrap();
    assert_eq!(
        m.run("root", false, &["pending", "k1"]).status.code(),
        Some(0)
    );
    assert_eq!(m.read("runuser.log"), "");
}

#[test]
fn a_runuser_that_leaves_it_someone_else_runs_it_once() {
    let m = Machine::new(Some("sam/homelab"));
    let ran = m.run_with(
        "root",
        false,
        &[("STAYS", "1")],
        &["open", "k1", "Stuck", "It's stuck."],
    );
    assert!(ran.status.success(), "{ran:?}");
    assert_eq!(m.read("runuser.log").lines().count(), 1);
    assert_eq!(
        m.read("journal"),
        "-t box-notify -p warning|box-notify open: runuser ran it as root, not sam\n"
    );
    assert_eq!(m.read("gh.log"), "");
}

#[test]
fn a_notice_github_doesnt_get_goes_to_the_journal_and_fails_nothing() {
    let m = Machine::new(Some("sam/homelab"));
    let ran = m.run("sam", true, &["open", "k1", "Stuck", "It's stuck."]);
    assert!(ran.status.success(), "{ran:?}");
    let journal = m.read("journal");
    assert!(
        journal.starts_with("-t box-notify -p warning|box-notify open k1: couldn't reach GitHub (GET repos/sam/homelab/issues?state=open&per_page=100: gh: HTTP 401: Bad credentials)"),
        "{journal}"
    );
    assert_eq!(
        m.run("sam", false, &["pending", "k1"]).status.code(),
        Some(1)
    );
}

#[test]
fn with_no_repo_it_only_writes_the_journal() {
    let m = Machine::new(None);
    assert!(
        m.run("sam", false, &["open", "k5", "Stuck", "It's stuck."])
            .status
            .success()
    );
    assert!(m.run("sam", false, &["close", "k5"]).status.success());
    assert_eq!(
        m.read("journal"),
        "-t box-notify -p warning|box-notify open k5 Stuck It's stuck.\n-t box-notify -p notice|box-notify close k5 Cleared.\n"
    );
    assert_eq!(m.read("gh.log"), "");
}

#[test]
fn what_it_cant_run() {
    let m = Machine::new(Some("sam/homelab"));
    for args in [
        &[][..],
        &["open", "k1", "Stuck"],
        &["shout", "k1"],
        &["pending", "../k1"],
    ] {
        let ran = m.run("root", false, args);
        assert_eq!(ran.status.code(), Some(2), "{args:?}");
        assert!(
            String::from_utf8_lossy(&ran.stderr).contains("usage:"),
            "{args:?}"
        );
    }
    let ran = Command::new(env!("CARGO_BIN_EXE_burrow-notify"))
        .output()
        .unwrap();
    assert_eq!(ran.status.code(), Some(2));
    // A config it can't read, or with a field it doesn't know.
    let ran = Command::new(env!("CARGO_BIN_EXE_burrow-notify"))
        .arg(m.path("missing.json"))
        .args(["pending", "k1"])
        .output()
        .unwrap();
    assert_eq!(ran.status.code(), Some(2));
    let mut config: serde_json::Value = serde_json::from_str(&m.read("config.json")).unwrap();
    config["labels"] = "updates".into();
    fs::write(m.path("config.json"), config.to_string()).unwrap();
    let ran = m.run("sam", false, &["open", "k1", "Stuck", "It's stuck."]);
    assert_eq!(ran.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&ran.stderr).contains("unknown field `labels`"));
    assert_eq!(m.read("gh.log"), "");
    assert_eq!(m.read("runuser.log"), "");
}
