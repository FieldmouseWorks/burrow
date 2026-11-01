//! burrow-update as update.sh runs it: the config file, and a stand-in for
//! runuser that runs git as it is and answers as gh from HOME/gh (with
//! HOME/gh.log keeping how gh was run), or fails as gh does when there's no
//! HOME/gh (with no token, so it isn't asked again). What each word does is train.rs's
//! tests; these are its words, where it says why, and its exits.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

const BIN: &str = env!("CARGO_BIN_EXE_burrow-update");

/// Written once for every test: a file being written while another test
/// starts a process is open for writing in that process until it execs,
/// and running the file then fails with "Text file busy".
fn runuser() -> PathBuf {
    static BIN: OnceLock<tempfile::TempDir> = OnceLock::new();
    let bin = BIN.get_or_init(|| {
        let bin = tempfile::tempdir().unwrap();
        let path = bin.path().join("runuser");
        fs::write(
            &path,
            r#"#!/bin/sh
[ "$1" = -u ] && [ "$2" = sam ] && [ "$3" = -- ] && [ "$4" = env ] || exit 9
shift 3
home=${2#HOME=}
if [ "$4" = gh ]; then
  echo "$*" >>"$home/gh.log"
  cat >/dev/null
  [ -e "$home/gh" ] || { echo "gh-token: no token for sam" >&2; exit 1; }
  printf 'HTTP/1.1 200 OK\n\n'
  exec cat "$home/gh"
fi
exec "$@"
"#,
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        bin
    });
    bin.path().join("runuser")
}

/// A machine whose config says `train`, with its clone (`work`) a repo of
/// two commits, the first of them main.
struct Machine {
    dir: tempfile::TempDir,
}

impl Machine {
    fn new(train: bool) -> Machine {
        let m = Machine {
            dir: tempfile::tempdir().unwrap(),
        };
        fs::create_dir(m.home()).unwrap();
        fs::create_dir(m.path("state")).unwrap();
        fs::write(
            m.home().join(".gitconfig"),
            "[user]\n\tname = test\n\temail = test@example.org\n",
        )
        .unwrap();
        let config = serde_json::json!({
            "update": "box-update", "host": "box", "state": m.path("state"), "work": m.path("work"),
            "user": "sam", "home": m.home(), "uid": 1000, "runuser": runuser(),
            "train": if train { serde_json::json!({"repo": "sam/homelab", "branch": "box-update",
                "settled": "review-settled", "context": "conary/train", "steward": "sams-steward"}) }
                else { serde_json::Value::Null },
        });
        fs::write(m.path("config.json"), config.to_string()).unwrap();
        fs::create_dir(m.path("work")).unwrap();
        m.git(&["init", "-q"]);
        for text in ["v1", "v2"] {
            fs::write(m.path("work/flake.lock"), text).unwrap();
            m.git(&["add", "flake.lock"]);
            m.git(&["commit", "-qm", text]);
        }
        m
    }

    fn path(&self, p: &str) -> PathBuf {
        self.dir.path().join(p)
    }

    fn home(&self) -> PathBuf {
        self.path("home")
    }

    fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(self.path("work"))
            .args(args)
            .env("HOME", self.home())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
        String::from_utf8_lossy(&out.stdout).trim_end().to_string()
    }

    fn run(&self, words: &[&str]) -> Output {
        Command::new(BIN)
            .arg(self.path("config.json"))
            .args(words)
            .output()
            .unwrap()
    }

    /// What gh answers.
    fn answers(&self, json: &str) {
        fs::write(self.home().join("gh"), json).unwrap();
    }
}

fn said(out: &Output) -> (Option<i32>, String, String) {
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn sha(c: char) -> String {
    c.to_string().repeat(40)
}

#[test]
fn a_usage_or_a_config_it_cant_take_is_neither_a_step_nor_a_refusal() {
    let m = Machine::new(true);
    let out = Command::new(BIN).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    let out = Command::new(BIN)
        .arg(m.path("none.json"))
        .arg("pr")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let usage = "usage: box-update-steps pr | red PR | ride PR | replaces PR MAIN [INPUT...] | replace PR | propose\n";
    for words in [
        &[][..],
        &["prs"],
        &["pr", "7"],
        &["ride"],
        &["ride", "7 abc"],
        &["red", "7"],
        &["replaces", &format!("7 {}", sha('a'))],
        &["replace"],
        &["propose", "now"],
    ] {
        assert_eq!(
            said(&m.run(words)),
            (Some(2), String::new(), usage.into()),
            "{words:?}"
        );
    }
    // A repo whose PRs don't land through its train has no PR of its own.
    let m = Machine::new(false);
    let (code, out, err) = said(&m.run(&["pr"]));
    assert_eq!((code, out.as_str()), (Some(2), ""));
    assert!(
        err.contains("its merge train, and this one's don't"),
        "{err}"
    );
}

#[test]
fn its_pr_is_said_as_update_sh_reads_it_and_a_failure_goes_to_the_journal() {
    let m = Machine::new(true);
    m.answers("[]");
    assert_eq!(
        said(&m.run(&["pr"])),
        (Some(0), String::new(), String::new())
    );
    let a = sha('a');
    m.answers(&format!(
        r#"[{{"number": 7, "head": {{"sha": "{a}", "repo": {{"full_name": "sam/homelab"}}}}}}]"#
    ));
    assert_eq!(
        said(&m.run(&["pr"])),
        (Some(0), format!("7 {a}\n"), String::new())
    );
    // gh as the machine's user, with its home and runtime dir.
    let home = m.home();
    let ran = fs::read_to_string(home.join("gh.log")).unwrap();
    assert_eq!(
        ran.lines().last(),
        Some(
            format!(
                "env HOME={} XDG_RUNTIME_DIR=/run/user/1000 gh api -i -X GET repos/sam/homelab/pulls?state=open&base=main&head=sam:box-update",
                home.display()
            )
            .as_str()
        )
    );
    fs::remove_file(home.join("gh")).unwrap();
    let (code, out, err) = said(&m.run(&["pr"]));
    assert_eq!((code, out.as_str()), (Some(1), ""));
    assert!(err.contains("gh-token: no token for sam"), "{err}");
}

#[test]
fn a_refusal_update_sh_says_goes_on_stdout() {
    let m = Machine::new(true);
    let a = sha('a');
    let pr = format!("7 {a}");
    // Nothing proposed: not red (GitHub isn't asked), and not ridden.
    assert_eq!(
        said(&m.run(&["red", &pr])),
        (Some(0), String::new(), String::new())
    );
    assert!(!m.home().join("gh.log").exists());
    assert_eq!(
        said(&m.run(&["ride", &pr])),
        (
            Some(1),
            format!("its PR #7 has a head it didn't push ({a}); holding until that lands\n"),
            String::new()
        )
    );
    // Its own head, red, with GitHub not saying: why on stdout, GitHub's
    // words in the journal.
    fs::write(m.path("state/proposed"), format!("{a}\n")).unwrap();
    let (code, out, err) = said(&m.run(&["red", &pr]));
    assert_eq!(
        (code, out.as_str()),
        (Some(1), "GitHub didn't say how its PR #7's checks stand\n")
    );
    assert!(err.contains("gh-token: no token for sam"), "{err}");
    let (code, out, _) = said(&m.run(&["replace", &pr]));
    assert_eq!(
        (code, out.as_str()),
        (
            Some(1),
            "GitHub didn't say how its PR #7's checks stand; its PR #7 stays as it is\n"
        )
    );
    // Red, as the steward reads it: "red", once its head is fetched (it
    // can't be, from a clone with no origin).
    m.answers(&format!(
        r#"{{"data": {{"repository": {{"pullRequest": {{"headRefOid": "{a}", "commits": {{"nodes": [{{"commit": {{"checkSuites": {{"nodes": [{{"conclusion": "STARTUP_FAILURE"}}]}}}}}}]}}}}}}}}}}"#
    ));
    let (code, out, _) = said(&m.run(&["red", &pr]));
    assert_eq!(
        (code, out),
        (Some(1), format!("couldn't fetch its PR #7's head ({a})\n"))
    );
}

#[test]
fn replaces_goes_only_when_a_fresh_bump_is_told_apart() {
    let m = Machine::new(true);
    let main = m.git(&["rev-parse", "HEAD~1"]);
    let head = m.git(&["rev-parse", "HEAD"]);
    // HEAD, a whole bump on main, other than the red head (main's first
    // commit here): it goes.
    let red = format!("7 {main}");
    assert_eq!(
        said(&m.run(&["replaces", &red, &main])),
        (Some(0), String::new(), String::new())
    );
    // Short of an input, its very tree, or one it can't tell apart: why,
    // and it doesn't.
    for (words, why) in [
        (
            vec!["replaces", &red, &main, "nixpkgs", "claude-code"],
            "a fresh bump is short of nixpkgs claude-code\n".to_string(),
        ),
        (
            vec!["replaces", &format!("7 {head}"), &main],
            "a fresh bump is that same tree\n".into(),
        ),
        (
            vec!["replaces", &red, &head],
            "a fresh bump has nothing new on main\n".into(),
        ),
    ] {
        assert_eq!(
            said(&m.run(&words)),
            (Some(1), why, String::new()),
            "{words:?}"
        );
    }
    let (code, out, _) = said(&m.run(&["replaces", &format!("7 {}", sha('b')), &main]));
    assert_eq!(code, Some(1));
    assert!(
        out.starts_with("a fresh bump can't be told apart from it"),
        "{out}"
    );
}

#[test]
fn a_pr_that_doesnt_open_goes_to_the_journal() {
    let m = Machine::new(true);
    // Its push goes nowhere: the clone has no origin.
    let (code, out, err) = said(&m.run(&["propose"]));
    assert_eq!((code, out.as_str()), (Some(1), ""));
    assert!(
        err.contains("git push -q --force origin HEAD:refs/heads/box-update"),
        "{err}"
    );
    assert!(!Path::new(&m.path("state/proposed")).exists());
}
