//! The pass as modules/ledger.nix runs it, with stand-ins for gh and cn on
//! PATH: gh lists the owners' repos the test gives it, and cn says, for a
//! sync, the lines and the exit the test gives it, as `cn sync --help` has
//! them. Each call of either is noted.

use std::fs;
use std::path::PathBuf;
use std::process::Output;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

struct Pass {
    dir: tempfile::TempDir,
}

impl Pass {
    /// Owners sam (sam/a, sam/b) and kim (kim/c), and a ledger not set up.
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(dir.path().join("ledger")).unwrap();
        burrow_test::executable(
            bin.join("gh"),
            r#"#!/bin/sh
echo "$*" >>"$FAKE/gh-calls"
[ "$1 $2" = "repo list" ] && [ -e "$FAKE/repos-$3" ] || exit 1
cat "$FAKE/repos-$3"
"#,
        );
        burrow_test::executable(
            bin.join("cn"),
            r#"#!/bin/sh
echo "$*" >>"$FAKE/cn-calls"
case $1 in
  init) [ ! -e "$FAKE/init-fails" ] || exit 1; touch "$CONARY_LEDGER/anchor" ;;
  sync) cat "$FAKE/says" 2>/dev/null; exit "$(cat "$FAKE/exit" 2>/dev/null || echo 0)" ;;
  *) exit 1 ;;
esac
"#,
        );
        let pass = Self { dir };
        pass.write("repos-sam", "sam/a\nsam/b\n");
        pass.write("repos-kim", "kim/c\n");
        let config = json!({"owners": ["sam", "kim"], "wake": pass.wake()});
        pass.write("config.json", &config.to_string());
        pass
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn write(&self, name: &str, text: &str) {
        fs::write(self.path(name), text).unwrap();
    }

    fn wake(&self) -> PathBuf {
        self.path("wake.json")
    }

    /// cn's sync says each `(repo, outcome)` and exits `exit`.
    fn says(&self, lines: &[(&str, &str)], exit: i32) {
        let lines: String = lines
            .iter()
            .map(|(repo, outcome)| {
                let mut line = json!({"repo": repo, "outcome": outcome});
                if *outcome == "done" {
                    line["prs"] = json!([]);
                    line["refused"] = json!([]);
                } else {
                    line["error"] = json!(format!("{repo} went wrong"));
                }
                format!("{line}\n")
            })
            .collect();
        self.write("says", &lines);
        self.write("exit", &exit.to_string());
    }

    fn run(&self, args: &[&str]) -> Output {
        let path = format!(
            "{}:{}",
            self.path("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        burrow_test::command(burrow_test::program!("burrow-ledger-sync"))
            .arg(self.path("config.json"))
            .args(args)
            .env("PATH", path)
            .env("FAKE", self.dir.path())
            .env("CONARY_LEDGER", self.path("ledger"))
            .output()
            .unwrap()
    }

    fn calls(&self, of: &str) -> Vec<String> {
        fs::read_to_string(self.path(&format!("{of}-calls")))
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    }

    fn booked(&self) -> Value {
        serde_json::from_str(&fs::read_to_string(self.wake()).unwrap()).unwrap()
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn passed(out: &Output) -> bool {
    out.status.success()
}

#[test]
fn a_full_pass_syncs_every_owners_repos_in_one_cn_sync() {
    let pass = Pass::new();
    pass.says(
        &[("sam/a", "done"), ("sam/b", "done"), ("kim/c", "done")],
        0,
    );
    let out = pass.run(&[]);
    assert!(passed(&out), "{}", stderr(&out));
    assert_eq!(pass.calls("cn"), ["init", "sync sam/a sam/b kim/c"]);
    assert_eq!(pass.calls("gh").len(), 2, "{:?}", pass.calls("gh"));
    // Each repo's line reaches the journal.
    assert_eq!(String::from_utf8_lossy(&out.stdout).lines().count(), 3);
    // And the journal says how many calls the pass made.
    assert!(
        stderr(&out).contains("syncing 3 repos in one cn sync"),
        "{}",
        stderr(&out)
    );
    // Set up, the ledger isn't set up again.
    let out = pass.run(&[]);
    assert!(passed(&out), "{}", stderr(&out));
    assert_eq!(pass.calls("cn")[2..], ["sync sam/a sam/b kim/c"]);
    assert!(!pass.wake().exists());
}

#[test]
fn one_repo_is_synced_alone_on_the_same_path() {
    let pass = Pass::new();
    pass.says(&[("sam/b", "done")], 0);
    let out = pass.run(&["sam/b"]);
    assert!(passed(&out), "{}", stderr(&out));
    assert_eq!(pass.calls("cn"), ["init", "sync sam/b"]);
    assert!(pass.calls("gh").is_empty(), "{:?}", pass.calls("gh"));
    // And booked, as in a full pass.
    pass.says(&[("sam/b", "unavailable")], 75);
    let out = pass.run(&["sam/b"]);
    assert!(passed(&out), "{}", stderr(&out));
    assert_eq!(pass.booked().as_object().unwrap().len(), 1);
}

#[test]
fn an_unavailable_repo_is_booked_beside_earlier_bookings_and_fails_nothing() {
    let pass = Pass::new();
    pass.write("wake.json", r#"{"sam/old": 1}"#);
    pass.says(
        &[
            ("sam/a", "done"),
            ("sam/b", "unavailable"),
            ("kim/c", "done"),
        ],
        75,
    );
    let before = now();
    let out = pass.run(&[]);
    assert!(passed(&out), "{}", stderr(&out));
    assert!(stderr(&out).contains("sam/b: GitHub is unavailable; again in five minutes"));
    let booked = pass.booked();
    assert_eq!(booked["sam/old"], 1, "{booked}");
    let at = booked["sam/b"].as_u64().unwrap();
    assert!(before + 300 <= at && at <= now() + 300, "{booked}");
    assert_eq!(booked.as_object().unwrap().len(), 2, "{booked}");
}

#[test]
fn a_failed_repo_is_named_and_fails_the_pass() {
    let pass = Pass::new();
    pass.says(
        &[
            ("sam/a", "done"),
            ("sam/b", "failed"),
            ("kim/c", "unavailable"),
        ],
        1,
    );
    let out = pass.run(&[]);
    assert!(!passed(&out));
    assert!(
        stderr(&out).contains("couldn't sync sam/b: sam/b went wrong"),
        "{}",
        stderr(&out)
    );
    assert!(
        !stderr(&out).contains("couldn't sync sam/a"),
        "{}",
        stderr(&out)
    );
    // The unavailable one is still booked.
    assert_eq!(pass.booked().as_object().unwrap().len(), 1);
    assert_eq!(pass.calls("cn").len(), 2);
}

#[test]
fn an_owner_that_cant_be_listed_fails_the_pass_and_the_others_sync() {
    let pass = Pass::new();
    fs::remove_file(pass.path("repos-sam")).unwrap();
    pass.says(&[("kim/c", "done")], 0);
    let out = pass.run(&[]);
    assert!(!passed(&out));
    assert!(
        stderr(&out).contains("couldn't list sam's repos"),
        "{}",
        stderr(&out)
    );
    assert_eq!(pass.calls("cn"), ["init", "sync kim/c"]);
}

#[test]
fn no_repos_is_no_sync() {
    let pass = Pass::new();
    pass.write("repos-sam", "");
    pass.write("repos-kim", "\n");
    let out = pass.run(&[]);
    assert!(passed(&out), "{}", stderr(&out));
    assert_eq!(pass.calls("cn"), ["init"]);
    // None listed at all fails the pass, still with no sync.
    fs::remove_file(pass.path("repos-sam")).unwrap();
    fs::remove_file(pass.path("repos-kim")).unwrap();
    let out = pass.run(&[]);
    assert!(!passed(&out));
    assert_eq!(pass.calls("cn"), ["init"]);
}

#[test]
fn a_booking_that_cant_be_written_fails_the_pass_and_the_file_stays() {
    for bad in ["", "  ", r#"{"sam/a": 1} {"sam/b": 2}"#, "[]", "not json"] {
        let pass = Pass::new();
        pass.write("wake.json", bad);
        pass.says(
            &[
                ("sam/a", "done"),
                ("sam/b", "unavailable"),
                ("kim/c", "done"),
            ],
            75,
        );
        let out = pass.run(&[]);
        assert!(!passed(&out), "{bad:?}");
        assert!(
            stderr(&out).contains("couldn't book sam/b's next pass"),
            "{}",
            stderr(&out)
        );
        assert_eq!(fs::read_to_string(pass.wake()).unwrap(), bad);
    }
}

#[test]
fn a_repo_cn_said_no_line_for_takes_its_exits_word() {
    // 75: GitHub was unavailable before it got to them (its token).
    let pass = Pass::new();
    pass.says(&[], 75);
    let out = pass.run(&[]);
    assert!(passed(&out), "{}", stderr(&out));
    let booked = pass.booked();
    assert_eq!(booked.as_object().unwrap().len(), 3, "{booked}");
    // Anything else: each one failed.
    let pass = Pass::new();
    pass.says(&[("sam/a", "done")], 1);
    let out = pass.run(&[]);
    assert!(!passed(&out));
    let err = stderr(&out);
    assert!(
        err.contains("couldn't sync sam/b: cn sync ended") && err.contains("couldn't sync kim/c"),
        "{err}"
    );
    assert!(!pass.wake().exists());
    // Even with an exit of 0.
    let pass = Pass::new();
    pass.says(&[("sam/a", "done"), ("kim/c", "done")], 0);
    assert!(!passed(&pass.run(&[])));
}

#[test]
fn lines_and_an_exit_that_disagree_fail_the_pass() {
    let cases: [(&[(&str, &str)], i32); 3] = [
        // 0, yet one wasn't done.
        (
            &[
                ("sam/a", "done"),
                ("sam/b", "unavailable"),
                ("kim/c", "done"),
            ],
            0,
        ),
        // 75, yet every one was done.
        (
            &[("sam/a", "done"), ("sam/b", "done"), ("kim/c", "done")],
            75,
        ),
        // A repo it wasn't asked about.
        (
            &[
                ("sam/a", "done"),
                ("sam/b", "done"),
                ("kim/c", "done"),
                ("sam/z", "done"),
            ],
            0,
        ),
    ];
    for (lines, exit) in cases {
        let pass = Pass::new();
        pass.says(lines, exit);
        assert!(!passed(&pass.run(&[])), "{lines:?} {exit}");
    }
    let pass = Pass::new();
    pass.says(&[], 0);
    pass.write("says", "not a line\n");
    assert!(!passed(&pass.run(&["sam/a"])));
}

#[test]
fn a_ledger_that_cant_be_set_up_fails_before_any_sync() {
    let pass = Pass::new();
    pass.write("init-fails", "");
    let out = pass.run(&[]);
    assert!(!passed(&out));
    assert_eq!(pass.calls("cn"), ["init"]);
    assert!(pass.calls("gh").is_empty());
}
