//! burrow-update as update.sh runs it: the config file, and a stand-in for
//! runuser that runs git as it is and answers as gh from HOME/gh (with
//! HOME/gh.log keeping how gh was run), or fails as gh does when there's no
//! HOME/gh (with no token, so it isn't asked again); as proj from HOME/proj
//! the same way (HOME/proj.log); and as a nix that builds nothing. What each
//! word does is the library's tests; these are its words, where it says
//! why, and its exits.

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
case $4 in
gh)
  echo "$*" >>"$home/gh.log"
  cat >/dev/null
  [ -e "$home/gh" ] || { echo "gh-token: no token for sam" >&2; exit 1; }
  printf 'HTTP/1.1 200 OK\n\n'
  exec cat "$home/gh" ;;
proj)
  echo "$*" >>"$home/proj.log"
  [ -e "$home/proj" ] || { echo "proj: no slot for it" >&2; exit 1; }
  exec cat "$home/proj" ;;
nix)
  echo "error: build failed" >&2
  exit 1 ;;
esac
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
        fs::create_dir_all(m.path("state/logs")).unwrap();
        fs::create_dir(m.path("state/jobs")).unwrap();
        fs::write(
            m.home().join(".gitconfig"),
            "[user]\n\tname = test\n\temail = test@example.org\n",
        )
        .unwrap();
        let config = serde_json::json!({
            "update": "box-update", "host": "box", "state": m.path("state"), "work": m.path("work"),
            "user": "sam", "home": m.home(), "uid": 1000, "runuser": runuser(),
            "configuration": "box", "system": "x86_64-linux", "heavy": null,
            "project": "homelab", "app": null,
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

    /// What proj answers.
    fn proj(&self, out: &str) {
        fs::write(self.home().join("proj"), out).unwrap();
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
    let usage = "usage: box-update-steps build | same CURRENT REV | main-runs MAIN CURRENT REV \
| runs-already NEW CURRENT REV MADE RIDING | live-through MAIN | live-at MAIN RUNNING \
| push-main | check NAME COMMIT LOG | dispatch KEY PROMPT \
| pr | red PR | ride PR | replaces PR MAIN [INPUT...] | replace PR | propose\n";
    let a = sha('a');
    for words in [
        &[][..],
        &["prs"],
        &["pr", "7"],
        &["ride"],
        &["ride", "7 abc"],
        &["red", "7"],
        &["replaces", &format!("7 {a}")],
        &["replace"],
        &["propose", "now"],
        &["build", "now"],
        &["same", "/nix/store/sys"],
        &["same", "/nix/store/sys", "abc"],
        &["main-runs", "main", "/nix/store/sys", ""],
        &["main-runs", &a, "/nix/store/sys", "ABC"],
        &[
            "runs-already",
            "/nix/store/new",
            "/nix/store/sys",
            "",
            "yes",
            "",
        ],
        &[
            "runs-already",
            "/nix/store/new",
            "/nix/store/sys",
            "",
            "",
            "#7",
        ],
        &["runs-already", "/nix/store/new", "/nix/store/sys", "", ""],
        &["live-through", "HEAD"],
        &["live-at", "main", "/nix/store/sys"],
        &["push-main", "now"],
        &["check", "../agents", &a, "log"],
        &["check", "agents", "abc", "log"],
        &["dispatch", "jobs/x", "prompt"],
        &["dispatch", "-x", "prompt"],
        &["dispatch", "build-main"],
    ] {
        assert_eq!(
            said(&m.run(words)),
            (Some(2), String::new(), usage.into()),
            "{words:?}"
        );
    }
    // A repo whose PRs don't land through its train has no PR of its own,
    // and the rest of a run as any other.
    let m = Machine::new(false);
    let (code, out, err) = said(&m.run(&["pr"]));
    assert_eq!((code, out.as_str()), (Some(2), ""));
    assert!(
        err.contains("its merge train, and this one's don't"),
        "{err}"
    );
    assert_eq!(
        said(&m.run(&["live-through", &a])),
        (Some(0), format!("{a}\n"), String::new())
    );
    // Its words before any of that: a usage, not a refusal.
    assert_eq!(
        said(&m.run(&["red", "7"])),
        (Some(2), String::new(), usage.into())
    );
}

#[test]
fn a_runs_steps_say_what_update_sh_reads_and_their_whys_go_where_it_puts_them() {
    let m = Machine::new(false);
    let (main, head) = (
        m.git(&["rev-parse", "HEAD~1"]),
        m.git(&["rev-parse", "HEAD"]),
    );
    // No build: nothing on stdout, nix's words in its log.
    assert_eq!(
        said(&m.run(&["build"])),
        (Some(1), String::new(), String::new())
    );
    assert_eq!(
        fs::read_to_string(m.path("state/logs/build.log")).unwrap(),
        "error: build failed\n"
    );
    assert_eq!(said(&m.run(&["same", "/nix/store/sys", ""])).0, Some(1));
    assert_eq!(
        said(&m.run(&["main-runs", &main, "/nix/store/sys", &main])).0,
        Some(0)
    );
    // What runs already: its verdict, or nothing.
    let sys = "/nix/store/sys";
    for (made, riding, verdict) in [
        ("1", "", "hold\n"),
        ("1", "7", "current\n"),
        ("", "", "current\n"),
    ] {
        assert_eq!(
            said(&m.run(&["runs-already", sys, sys, &head, made, riding])),
            (Some(0), verdict.into(), String::new()),
            "{made:?} {riding:?}"
        );
    }
    assert_eq!(
        said(&m.run(&["runs-already", "/nix/store/new", sys, &head, "", ""])),
        (Some(0), String::new(), String::new())
    );
    assert_eq!(
        said(&m.run(&["live-at", &main, sys])),
        (Some(0), String::new(), String::new())
    );
    fs::remove_dir_all(m.path("state")).unwrap();
    let (code, out, err) = said(&m.run(&["live-at", &main, sys]));
    assert_eq!((code, out.as_str()), (Some(1), ""));
    assert!(err.contains("live.json"), "{err}");
    // A push that doesn't go: why on stdout, for the journal.
    let (code, out, _) = said(&m.run(&["push-main"]));
    assert_eq!(
        (code, out.as_str()),
        (
            Some(1),
            "push to main was refused 1 times; the next run bumps it afresh\n"
        )
    );
}

#[test]
fn a_problem_handed_over_is_said_and_one_that_isnt_goes_on() {
    let m = Machine::new(false);
    m.proj("homelab-1\n");
    assert_eq!(
        said(&m.run(&["dispatch", "build-main", "main doesn't build"])),
        (
            Some(0),
            "handed to Claude: proj log homelab-1\n".into(),
            String::new()
        )
    );
    assert_eq!(
        fs::read_to_string(m.home().join("proj.log")).unwrap(),
        format!(
            "env HOME={} XDG_RUNTIME_DIR=/run/user/1000 proj run --pipeline homelab main doesn't build\n",
            m.home().display()
        )
    );
    fs::remove_file(m.home().join("proj")).unwrap();
    let (code, out, err) = said(&m.run(&["dispatch", "health", "it broke"]));
    assert_eq!((code, out.as_str()), (Some(0), ""));
    assert!(err.contains("proj: no slot for it"), "{err}");
    // A VM test that doesn't build doesn't pass.
    assert_eq!(
        said(&m.run(&[
            "check",
            "agents",
            &m.git(&["rev-parse", "HEAD"]),
            m.path("check.log").to_str().unwrap()
        ])),
        (Some(1), String::new(), String::new())
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
