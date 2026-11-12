//! claude-trust on a stand-in ~/.claude.json: each folder trusted under the
//! spelling given and its real path, the rest of the config kept, a folder
//! not made yet trusted at its path, nothing written when it's trusted
//! already, and Claude Code's own lock on the config kept to: waited for,
//! taken over once it's stale, never written through.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};

struct Case {
    dir: tempfile::TempDir,
}

impl Case {
    fn new() -> Case {
        let case = Case {
            dir: tempfile::tempdir().unwrap(),
        };
        fs::create_dir_all(case.path("home")).unwrap();
        fs::create_dir_all(case.path("real/project")).unwrap();
        symlink(case.path("real"), case.path("link")).unwrap();
        case
    }

    fn path(&self, p: &str) -> PathBuf {
        self.dir.path().join(p)
    }

    fn cfg(&self) -> PathBuf {
        self.path("home/.claude.json")
    }

    fn lock(&self) -> PathBuf {
        self.path("home/.claude.json.lock")
    }

    fn run(&self, dirs: &[PathBuf]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_claude-trust"))
            .args(dirs)
            .env_clear()
            .env("HOME", self.path("home"))
            .output()
            .unwrap()
    }

    fn config(&self) -> Value {
        serde_json::from_str(&fs::read_to_string(self.cfg()).unwrap()).unwrap()
    }

    fn trusted(&self, p: &Path) -> bool {
        self.config()["projects"][p.to_str().unwrap()]["hasTrustDialogAccepted"] == true
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn no_config_says_to_log_in_and_makes_none() {
    let case = Case::new();
    let out = case.run(&[case.path("link/project")]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("log in"), "{}", stderr(&out));
    assert!(!case.cfg().exists());
}

#[test]
fn no_arguments_is_a_usage_error() {
    let case = Case::new();
    let out = case.run(&[]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("usage"));
}

#[test]
fn both_spellings_trusted_and_the_rest_kept() {
    let case = Case::new();
    fs::write(
        case.cfg(),
        r#"{"projects":{"/elsewhere":{"allowedTools":["x"]}},"numStartups":3}"#,
    )
    .unwrap();
    fs::set_permissions(case.cfg(), fs::Permissions::from_mode(0o644)).unwrap();

    let out = case.run(&[case.path("link/project"), case.path("later/claude")]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        case.trusted(&case.path("link/project")),
        "the spelling given"
    );
    assert!(case.trusted(&case.path("real/project")), "the real path");
    assert!(
        case.trusted(&case.path("later/claude")),
        "a folder not made yet"
    );
    let config = case.config();
    assert_eq!(config["numStartups"], 3);
    assert_eq!(
        config["projects"]["/elsewhere"],
        json!({"allowedTools": ["x"]})
    );
    assert_eq!(fs::metadata(case.cfg()).unwrap().mode() & 0o777, 0o600);
    assert!(!case.lock().exists(), "left its lock");
}

#[test]
fn dot_dot_through_a_link_and_past_a_missing_folder_is_folded() {
    let case = Case::new();
    fs::write(case.cfg(), "{}").unwrap();
    let out = case.run(&[
        case.path("link/project/../project"),
        case.path("none/../later"),
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(case.trusted(&case.path("real/project")));
    assert!(case.trusted(&case.path("later")));
}

#[test]
fn trusted_already_writes_nothing_even_with_the_lock_held() {
    let case = Case::new();
    fs::write(case.cfg(), "{}").unwrap();
    assert!(case.run(&[case.path("link/project")]).status.success());
    fs::create_dir(case.lock()).unwrap();
    let before = fs::metadata(case.cfg()).unwrap().ino();
    let out = case.run(&[case.path("link/project")]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        fs::metadata(case.cfg()).unwrap().ino(),
        before,
        "rewrote it"
    );
}

#[test]
fn a_live_writers_lock_is_waited_on_then_given_up_on() {
    let case = Case::new();
    fs::write(case.cfg(), "{}").unwrap();
    fs::create_dir(case.lock()).unwrap();
    let out = case.run(&[case.path("other")]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("stayed locked"), "{}", stderr(&out));
    assert!(
        !case.trusted(&case.path("other")),
        "wrote through a held lock"
    );
    assert!(case.lock().is_dir(), "took a live writer's lock away");
}

#[test]
fn a_lock_a_crash_left_behind_is_taken_over() {
    let case = Case::new();
    fs::write(case.cfg(), "{}").unwrap();
    fs::create_dir(case.lock()).unwrap();
    let old = SystemTime::now() - Duration::from_secs(60);
    fs::File::open(case.lock())
        .unwrap()
        .set_modified(old)
        .unwrap();
    let out = case.run(&[case.path("other")]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        case.trusted(&case.path("other")),
        "a stale lock kept it out"
    );
    assert!(!case.lock().exists(), "left the lock it took over");
}
