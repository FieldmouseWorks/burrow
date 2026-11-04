//! renovate-run on a machine made of files, with stand-ins for gh,
//! gh-token, Renovate and its autodiscoverFilter: which repos get a run,
//! with whose token. As the user, gh's login covers every repo in one run;
//! as the machine's own App, each repo gets a run of its own with a token of
//! its owner's installation, asked for that run once it holds the runs'
//! lock. Each run that ends, failed or not, is noted for its repo (or
//! `all`), the last ten.
//!
//! The stand-ins note how they were run in FAKE/log, and whether the runs'
//! lock was held as they were (by the run that called them: nothing else
//! takes it here). A file in FAKE makes one fail or answer otherwise.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

const RENOVATE_RUN: &str = env!("CARGO_BIN_EXE_burrow-renovate");

/// Written once for every test: a file being written while another test
/// starts a process is open for writing in that process until it execs,
/// and running the file then fails with "Text file busy".
fn bin() -> &'static Path {
    static BIN: OnceLock<tempfile::TempDir> = OnceLock::new();
    BIN.get_or_init(|| {
        let bin = tempfile::tempdir().unwrap();
        for (name, text) in STAND_INS {
            let path = bin.path().join(name);
            fs::write(&path, format!("#!/bin/sh\n{LOCKED}{text}")).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        bin
    })
    .path()
}

/// Whether the runs' lock is held as WHAT is asked for.
const LOCKED: &str = r#"locked() {
  if flock -n "$FAKE/lock" true; then echo "unlocked $1" >>"$FAKE/log"; fi
}
"#;

const STAND_INS: [(&str, &str); 4] = [
    (
        // GitHub: a repo as it spells it (none for OWNER/gone), and what
        // each owner's installation reaches (FAKE/OWNER-repos, or
        // FAKE/OWNER-fails to fail).
        "gh",
        r#"echo "gh $* owner=${GH_TOKEN_OWNER-} socket=${GH_TOKEN_SOCKET-}" >>"$FAKE/log"
case "$1 $2" in
  "auth token") locked token; echo gho_user ;;
  "api repos/"*)
    case $2 in
      repos/sam/r) echo Sam/R ;;
      */gone) echo "gh: Not Found (HTTP 404)" >&2; exit 1 ;;
      *) echo "${2#repos/}" ;;
    esac ;;
  "api installation/repositories")
    [ ! -e "$FAKE/$GH_TOKEN_OWNER-fails" ] || exit 1
    cat "$FAKE/$GH_TOKEN_OWNER-repos" ;;
  *) echo "gh $*: not a stand-in answer" >&2; exit 1 ;;
esac
"#,
    ),
    (
        // The broker: refuses without its socket, as the real one does,
        // and hands out a new token each time, so a run's own is told
        // apart. FAKE/gh-token-fails makes it fail with that exit.
        "gh-token",
        r#"[ -n "${GH_TOKEN_SOCKET-}" ] || { echo "GH_TOKEN_SOCKET isn't set" >&2; exit 1; }
echo "gh-token $*" >>"$FAKE/log"
locked token
[ ! -e "$FAKE/gh-token-fails" ] || exit "$(cat "$FAKE/gh-token-fails")"
echo "ghs_$2_$(wc -l <"$FAKE/log")"
"#,
    ),
    (
        // Renovate: what it was run with, and with whose token. A run with
        // an argument holding what FAKE/renovate-fails says fails.
        "renovate",
        r#"echo "renovate $* token=$RENOVATE_TOKEN com=$GITHUB_COM_TOKEN" >>"$FAKE/log"
locked renovate
[ ! -e "$FAKE/renovate-fails" ] || case "$*" in *"$(cat "$FAKE/renovate-fails")"*) exit 1 ;; esac
"#,
    ),
    (
        // autodiscoverFilter: takes everything but what FAKE/uncovered
        // names, and can't be applied to OWNER/broken.
        "covers",
        r#"echo "covers $*" >>"$FAKE/log"
case $1 in */broken) exit 2 ;; esac
for r in $(cat "$FAKE/uncovered" 2>/dev/null); do [ "$r" != "$1" ] || exit 10; done
"#,
    ),
];

struct Machine {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

/// What a run exited with, and said on stdout and stderr.
type Said = (Option<i32>, String, String);

impl Machine {
    /// As the user, or as the App of `owners`.
    fn new(app: Option<&[&str]>) -> Machine {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        fs::create_dir(root.join("fake")).unwrap();
        let m = Machine { _dir: dir, root };
        let b = bin();
        let app = app.map(|owners| {
            serde_json::json!({
                "env": {"GH_TOKEN_SOCKET": "/run/gh-token/workers.sock"},
                "owners": owners, "ghToken": b.join("gh-token"),
            })
        });
        m.config(serde_json::json!({
            "name": "renovate-run", "runLock": m.path("fake/lock"), "runs": m.path("runs"),
            "app": app,
            "tools": {"gh": b.join("gh"), "renovate": b.join("renovate"), "covers": b.join("covers")},
        }));
        m
    }

    fn path(&self, p: &str) -> PathBuf {
        self.root.join(p)
    }

    fn config(&self, config: serde_json::Value) {
        fs::write(self.path("config.json"), config.to_string()).unwrap();
    }

    /// FAKE/NAME, which makes a stand-in fail or answer otherwise.
    fn set(&self, name: &str, text: &str) {
        fs::write(self.path("fake").join(name), text).unwrap();
    }

    fn run(&self, words: &[&str]) -> Said {
        let out = Command::new(RENOVATE_RUN)
            .arg(self.path("config.json"))
            .args(words)
            .env("FAKE", self.path("fake"))
            .env_remove("GH_TOKEN_SOCKET")
            .env_remove("GH_TOKEN_OWNER")
            .output()
            .unwrap();
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    /// Every call the stand-ins took since this was last asked, in order.
    /// Every token is asked for, and every run made, holding the lock: a
    /// token asked for before a wait for it could run out before its run
    /// does.
    fn calls(&self) -> Vec<String> {
        let path = self.path("fake/log");
        let text = fs::read_to_string(&path).unwrap_or_default();
        let _ = fs::remove_file(path);
        let calls: Vec<String> = text.lines().map(str::to_string).collect();
        assert!(
            !calls.iter().any(|c| c.starts_with("unlocked ")),
            "{calls:#?}"
        );
        calls
    }

    /// Renovate's runs since the calls were last asked for: how each was
    /// run, its tokens aside.
    fn runs(&self) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter_map(|c| {
                c.strip_prefix("renovate")
                    .map(|r| r.split(" token=").next().unwrap().trim().to_string())
            })
            .collect()
    }

    /// The names runs are noted for.
    fn noted(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.path("runs"))
            .map(|d| {
                d.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    /// When the runs noted for `name` ended.
    fn ended(&self, name: &str) -> Vec<u64> {
        fs::read_to_string(self.path("runs").join(name))
            .unwrap_or_default()
            .lines()
            .map(|l| l.parse().unwrap())
            .collect()
    }
}

fn ok(out: &str) -> Said {
    (Some(0), out.into(), String::new())
}

fn filter(repo: &str) -> String {
    format!(r#"--autodiscover-filter=["{repo}"]"#)
}

#[test]
fn a_usage_or_a_config_it_cant_take_exits_2_having_run_nothing() {
    let m = Machine::new(None);
    for words in [&["sam"][..], &["sam/r", "now"], &["sam/r/x"], &[""]] {
        assert_eq!(
            m.run(words),
            (
                Some(2),
                String::new(),
                "usage: renovate-run [OWNER/REPO]\n".into()
            ),
            "{words:?}"
        );
    }
    m.config(serde_json::json!({"name": "renovate-run"}));
    let (code, out, err) = m.run(&[]);
    assert_eq!((code, out.as_str()), (Some(2), ""));
    assert!(err.starts_with("burrow-renovate: "), "{err}");
    assert_eq!(m.calls(), Vec::<String>::new());
    assert_eq!(m.noted(), Vec::<String>::new());
}

#[test]
fn as_the_user_one_run_covers_every_repo_with_its_login() {
    let m = Machine::new(None);
    assert_eq!(m.run(&[]), ok(""));
    assert_eq!(
        m.calls(),
        [
            "gh auth token owner= socket=",
            "renovate  token=gho_user com=gho_user"
        ]
    );
    assert_eq!(m.noted(), ["all"]);
    assert_eq!(m.ended("all").len(), 1);
}

#[test]
fn one_repo_runs_as_github_spells_it_if_the_filter_covers_it() {
    let m = Machine::new(None);
    assert_eq!(m.run(&["sam/r"]), ok(""));
    assert_eq!(
        m.calls(),
        [
            "gh api repos/sam/r --jq .full_name owner= socket=".to_string(),
            "covers Sam/R".into(),
            "gh auth token owner= socket=".into(),
            format!("renovate {} token=gho_user com=gho_user", filter("Sam/R")),
        ]
    );
    // Noted by its name in lower case.
    assert_eq!(m.noted(), ["sam_r"]);
    assert_eq!(m.ended("sam_r").len(), 1);

    let m = Machine::new(None);
    m.set("uncovered", "Sam/R");
    assert_eq!(
        m.run(&["sam/r"]),
        ok("Sam/R: not one of Renovate's (autodiscoverFilter)\n")
    );
    assert_eq!(m.runs(), Vec::<String>::new());
    assert_eq!(m.noted(), Vec::<String>::new());

    let m = Machine::new(None);
    assert_eq!(
        m.run(&["sam/broken"]),
        (
            Some(1),
            String::new(),
            "sam/broken: couldn't apply autodiscoverFilter\n".into()
        )
    );
    assert_eq!(m.runs(), Vec::<String>::new());
    assert_eq!(m.noted(), Vec::<String>::new());

    // GitHub's own word on a repo it doesn't have.
    assert_eq!(
        m.run(&["sam/gone"]),
        (Some(1), String::new(), "gh: Not Found (HTTP 404)\n".into())
    );
    assert_eq!(
        m.calls(),
        ["gh api repos/sam/gone --jq .full_name owner= socket="]
    );
}

#[test]
fn a_run_that_fails_is_noted_too_and_only_the_last_ten_are_kept() {
    let m = Machine::new(None);
    m.set("renovate-fails", "Sam/R");
    assert_eq!(m.run(&["sam/r"]), (Some(1), String::new(), String::new()));
    assert_eq!(m.ended("sam_r").len(), 1);
    fs::remove_file(m.path("fake/renovate-fails")).unwrap();
    for _ in 0..10 {
        assert_eq!(m.run(&["sam/r"]), ok(""));
    }
    let ended = m.ended("sam_r");
    assert_eq!(ended.len(), 10);
    assert!(ended.is_sorted(), "{ended:?}");
    assert_eq!(m.noted(), ["sam_r"]);
}

#[test]
fn as_the_app_one_repos_run_gets_its_owners_token() {
    let m = Machine::new(Some(&[]));
    assert_eq!(m.run(&["sam/r"]), ok(""));
    let calls = m.calls();
    // Every call is the App's: its env is the run's.
    assert_eq!(
        calls[..3],
        [
            "gh api repos/sam/r --jq .full_name owner= socket=/run/gh-token/workers.sock",
            "covers Sam/R",
            "gh-token get Sam"
        ]
    );
    // Its token, never gh's login.
    let token = calls[3]
        .strip_prefix(&format!("renovate {} token=", filter("Sam/R")))
        .unwrap();
    let (token, com) = token.split_once(" com=").unwrap();
    assert!(token.starts_with("ghs_Sam_"), "{token}");
    assert_eq!(com, token);
    assert_eq!(calls.len(), 4);
    assert_eq!(m.noted(), ["sam_r"]);

    m.set("uncovered", "Sam/R");
    assert_eq!(
        m.run(&["sam/r"]),
        ok("Sam/R: not one of Renovate's (autodiscoverFilter)\n")
    );
    assert!(!m.calls().iter().any(|c| c.starts_with("gh-token")));
}

#[test]
fn a_token_it_cant_have_is_no_run() {
    let m = Machine::new(Some(&[]));
    m.set("gh-token-fails", "3");
    assert_eq!(m.run(&["sam/r"]), (Some(3), String::new(), String::new()));
    assert_eq!(m.runs(), Vec::<String>::new());
    assert_eq!(m.noted(), Vec::<String>::new());
}

#[test]
fn a_full_run_as_the_app_is_a_run_per_repo_each_with_its_owners_token() {
    let m = Machine::new(Some(&["sam", "kim"]));
    m.set("sam-repos", "sam/a\nsam/b\n");
    m.set("kim-repos", "kim/c\n");
    m.set("uncovered", "sam/b");
    assert_eq!(
        m.run(&[]),
        ok("sam/b: not one of Renovate's (autodiscoverFilter)\n")
    );
    let calls = m.calls();
    let listed: Vec<&String> = calls
        .iter()
        .filter(|c| c.starts_with("gh api installation/repositories"))
        .collect();
    assert_eq!(
        listed,
        [
            "gh api installation/repositories --paginate --jq .repositories[] | select(.archived | not) | .full_name owner=sam socket=/run/gh-token/workers.sock",
            "gh api installation/repositories --paginate --jq .repositories[] | select(.archived | not) | .full_name owner=kim socket=/run/gh-token/workers.sock",
        ]
    );
    let runs: Vec<&String> = calls
        .iter()
        .filter(|c| c.starts_with("renovate "))
        .collect();
    assert_eq!(runs.len(), 2);
    assert!(runs[0].starts_with(&format!("renovate {} token=ghs_sam_", filter("sam/a"))));
    assert!(runs[1].starts_with(&format!("renovate {} token=ghs_kim_", filter("kim/c"))));
    assert_eq!(m.noted(), ["kim_c", "sam_a"]);

    // A token asked for again for each run, so none outlives its token.
    let m = Machine::new(Some(&["sam"]));
    m.set("sam-repos", "sam/a\nsam/b\n");
    assert_eq!(m.run(&[]), ok(""));
    let tokens: Vec<String> = m
        .calls()
        .iter()
        .filter_map(|c| {
            c.split(" token=")
                .nth(1)
                .map(|t| t.split(' ').next().unwrap().to_string())
        })
        .collect();
    assert_eq!(tokens.len(), 2);
    assert_ne!(tokens[0], tokens[1]);
}

#[test]
fn a_repo_or_an_owner_that_fails_holds_up_none_of_the_rest_but_fails_the_pass() {
    let m = Machine::new(Some(&["sam", "kim"]));
    m.set("sam-repos", "sam/a\nsam/b\n");
    m.set("kim-repos", "kim/c\n");
    m.set("renovate-fails", "sam/a");
    assert_eq!(
        m.run(&[]),
        (
            Some(1),
            String::new(),
            "sam/a: Renovate's run failed\n".into()
        )
    );
    assert_eq!(
        m.runs(),
        [filter("sam/a"), filter("sam/b"), filter("kim/c")]
    );
    assert_eq!(m.noted(), ["kim_c", "sam_a", "sam_b"]);

    let m = Machine::new(Some(&["sam", "kim"]));
    m.set("kim-repos", "kim/c\n");
    m.set("sam-fails", "");
    assert_eq!(
        m.run(&[]),
        (
            Some(1),
            String::new(),
            "couldn't list the repos sam's installation reaches\n".into()
        )
    );
    assert_eq!(m.runs(), [filter("kim/c")]);

    let m = Machine::new(Some(&["sam"]));
    m.set("sam-repos", "sam/broken\nsam/a\n");
    assert_eq!(
        m.run(&[]),
        (
            Some(1),
            String::new(),
            "sam/broken: couldn't apply autodiscoverFilter\nsam/broken: Renovate's run failed\n"
                .into()
        )
    );
    // Never one run over everything with one token.
    assert_eq!(m.runs(), [filter("sam/a")]);
}

#[test]
fn what_it_cant_lock_or_note_it_says_and_exits_1() {
    let m = Machine::new(None);
    let lock = m.path("nowhere/run.lock");
    m.config(serde_json::json!({
        "name": "renovate-run", "runLock": lock, "runs": m.path("runs"), "app": null,
        "tools": {"gh": bin().join("gh"), "renovate": bin().join("renovate"), "covers": bin().join("covers")},
    }));
    let (code, out, err) = m.run(&[]);
    assert_eq!((code, out.as_str()), (Some(1), ""));
    assert!(
        err.starts_with(&format!("renovate-run: {}: ", lock.display())),
        "{err}"
    );
    assert_eq!(m.runs(), Vec::<String>::new());

    // A run that ran, but couldn't be noted.
    let m = Machine::new(None);
    fs::write(m.path("runs"), "").unwrap();
    let (code, _, err) = m.run(&[]);
    assert_eq!(code, Some(1));
    assert!(
        err.starts_with(&format!("renovate-run: {}: ", m.path("runs").display())),
        "{err}"
    );
    assert_eq!(m.runs(), [""]);
}
