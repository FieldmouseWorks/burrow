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
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

const RENOVATE_RUN: &str = env!("CARGO_BIN_EXE_burrow-renovate");

/// Shared stand-ins, installed by burrow_test and cached for the test binary.
fn bin() -> &'static Path {
    static BIN: OnceLock<tempfile::TempDir> = OnceLock::new();
    BIN.get_or_init(|| {
        let bin = tempfile::tempdir().unwrap();
        for (name, text) in STAND_INS {
            let path = bin.path().join(name);
            burrow_test::executable(&path, format!("#!/bin/sh\n{LOCKED}{text}"));
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
        // GitHub: a repo as it spells it (none for OWNER/gone), what each
        // owner's installation reaches (FAKE/OWNER-repos, or FAKE/OWNER-fails
        // to fail), and its allowance: spent till the time in
        // FAKE/OWNER-spent (FAKE/user-spent for the user's login).
        "gh",
        r#"echo "gh $* owner=${GH_TOKEN_OWNER-} socket=${GH_TOKEN_SOCKET-}" >>"$FAKE/log"
if [ "$1" = api ]; then
  if [ -e "$FAKE/once-502" ]; then
    rm "$FAKE/once-502"
    printf 'HTTP/2.0 502 Bad Gateway\n\n{}'; exit 1
  fi
  [ "$2 $3 $4" = '-i -X GET' ] || exit 2
  path=$5
  # GitHub's rate limit, till the time in FAKE/OWNER-limited (an owner's
  # installation's list) or FAKE/OWNER_REPO-limited (a repo).
  case "$path" in
    installation/repositories) limited=$FAKE/$GH_TOKEN_OWNER-limited ;;
    repos/*/*) limited=$(echo "${path#repos/}" | tr / _); limited=$FAKE/$limited-limited ;;
  esac
  if [ -e "$limited" ]; then
    printf 'HTTP/2.0 403 Forbidden\nx-ratelimit-remaining: 0\nx-ratelimit-reset: %s\n\n{"message":"API rate limit exceeded for installation ID 1."}' "$(cat "$limited")"
    exit 1
  fi
  case "$path" in
    rate_limit)
      spent=$FAKE/${GH_TOKEN_OWNER:-user}-spent
      left=4999 reset=1
      [ ! -e "$spent" ] || { left=0; reset=$(cat "$spent"); }
      printf 'HTTP/2.0 200 OK\n\n{"resources":{"core":{"remaining":%s,"reset":%s},"graphql":{"remaining":5000,"reset":1}}}' "$left" "$reset" ;;
    repos/*)
      case "$path" in
        repos/sam/r) repo=Sam/R ;;
        */gone) printf 'HTTP/2.0 404 Not Found\n\n{"message":"Not Found"}'; exit 1 ;;
        *) repo=${path#repos/} ;;
      esac
      printf 'HTTP/2.0 200 OK\n\n{"full_name":"%s"}' "$repo" ;;
    installation/repositories)
      [ ! -e "$FAKE/$GH_TOKEN_OWNER-fails" ] || { printf 'HTTP/2.0 403 Forbidden\n\n{"message":"installation access denied"}'; exit 1; }
      printf 'HTTP/2.0 200 OK\n\n'
      jq -Rs '{repositories: [split("\n")[] | select(length>0) | {full_name:.,archived:false}]}' "$FAKE/$GH_TOKEN_OWNER-repos" ;;
    *) exit 2 ;;
  esac
else
  case "$1 $2" in "auth token") locked token; echo gho_user ;; *) exit 2 ;; esac
fi
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
# FAKE/renovate-outlives: renovate-run is killed while it runs, and the lock
# is still held once it's gone.
if [ -e "$FAKE/renovate-outlives" ]; then
  kill -9 "$PPID"
  while [ -e "/proc/$PPID" ] && [ "$(cut -d' ' -f3 "/proc/$PPID/stat")" != Z ]; do sleep 0.05; done
  locked "renovate after renovate-run"
fi
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
            "wake": m.path("wake.json"), "app": app,
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

    /// The passes booked in the wake file: none when there's none.
    fn booked(&self) -> serde_json::Value {
        fs::read(self.path("wake.json"))
            .map(|b| serde_json::from_slice(&b).unwrap())
            .unwrap_or(serde_json::json!({}))
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
fn github_recovers_after_502() {
    let m = Machine::new(None);
    m.set("once-502", "");
    assert_eq!(m.run(&["sam/r"]), ok(""));
    let calls = m.calls();
    assert_eq!(
        calls
            .iter()
            .filter(|c| c.starts_with("gh api -i -X GET repos/sam/r "))
            .count(),
        2
    );
    assert!(calls.iter().any(|c| c.starts_with("renovate ")));
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
fn renovate_holds_the_lock_till_its_gone_whatever_becomes_of_its_run() {
    for app in [None, Some(&["sam"][..])] {
        for words in [&[][..], &["sam/r"]] {
            let m = Machine::new(app);
            m.set("sam-repos", "sam/r\n");
            m.set("renovate-outlives", "");
            let (code, _, _) = m.run(words);
            assert_eq!(code, None, "{app:?} {words:?}: killed");
            // calls() fails on an unlocked call.
            assert!(m.runs().len() == 1, "{app:?} {words:?}");
        }
    }
}

#[test]
fn one_repo_runs_as_github_spells_it_if_the_filter_covers_it() {
    let m = Machine::new(None);
    assert_eq!(m.run(&["sam/r"]), ok(""));
    assert_eq!(
        m.calls(),
        [
            "gh api -i -X GET repos/sam/r owner= socket=".to_string(),
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
        (
            Some(1),
            String::new(),
            "GET repos/sam/gone: HTTP 404: Not Found\n".into()
        )
    );
    assert_eq!(
        m.calls(),
        ["gh api -i -X GET repos/sam/gone owner= socket="]
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
            "gh api -i -X GET repos/sam/r owner= socket=/run/gh-token/workers.sock",
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
        .filter(|c| c.starts_with("gh api -i -X GET installation/repositories"))
        .collect();
    assert_eq!(
        listed,
        [
            "gh api -i -X GET installation/repositories owner=sam socket=/run/gh-token/workers.sock",
            "gh api -i -X GET installation/repositories owner=kim socket=/run/gh-token/workers.sock",
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
            "GET installation/repositories: HTTP 403: installation access denied\ncouldn't list the repos sam's installation reaches\n".into()
        )
    );
    assert_eq!(m.runs(), [filter("kim/c")]);
    // GitHub's no is a failure, not a wait: nothing is booked.
    assert_eq!(m.booked(), serde_json::json!({}));

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
        "name": "renovate-run", "runLock": lock, "runs": m.path("runs"), "wake": m.path("wake.json"), "app": null,
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

/// An hour from now, as GitHub's x-ratelimit-reset has it.
fn in_an_hour() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600
}

/// What renovate-run says of a pass it booked again for AT.
fn booked_again(what: &str, at: u64) -> String {
    let at = jiff::Timestamp::from_second(at as i64).unwrap();
    format!("{what}: booked again for {at}, when GitHub's rate limit lifts")
}

#[test]
fn a_repo_github_rate_limits_is_booked_again_for_when_it_lifts_not_failed() {
    for app in [None, Some(&["sam"][..])] {
        let m = Machine::new(app);
        let at = in_an_hour();
        m.set("sam_r-limited", &at.to_string());
        assert_eq!(
            m.run(&["sam/r"]),
            ok(&format!(
                "{}: GET repos/sam/r: HTTP 403: API rate limit exceeded for installation ID 1.\n",
                booked_again("sam/r", at)
            )),
            "{app:?}"
        );
        assert_eq!(m.runs(), Vec::<String>::new());
        assert_eq!(m.noted(), Vec::<String>::new());
        assert_eq!(m.booked(), serde_json::json!({"sam/r": at}));

        // The pass burrow-events starts for it once it's due uses it up, and
        // leaves the rest.
        fs::remove_file(m.path("fake/sam_r-limited")).unwrap();
        fs::write(m.path("wake.json"), r#"{"sam/r": 1, "kim/q": 5}"#).unwrap();
        assert_eq!(m.run(&["sam/r"]), ok(""));
        assert_eq!(m.runs(), [filter("Sam/R")]);
        assert_eq!(m.booked(), serde_json::json!({"kim/q": 5}));
    }
}

#[test]
fn a_full_run_books_what_the_rate_limit_held_off_and_runs_the_rest() {
    let m = Machine::new(Some(&["sam", "kim", "lee"]));
    let (at, later) = (in_an_hour(), in_an_hour() + 600);
    m.set("sam-repos", "sam/a\nsam/b\nsam/c\n");
    m.set("sam_b-limited", &at.to_string());
    m.set("kim-limited", &later.to_string());
    m.set("lee-repos", "lee/d\n");
    assert_eq!(
        m.run(&[]),
        ok(&format!(
            "{}: GET repos/sam/b: HTTP 403: API rate limit exceeded for installation ID 1.\n{}\n{}: GET installation/repositories: HTTP 403: API rate limit exceeded for installation ID 1.\n",
            booked_again("sam/b", at),
            booked_again("sam/c", at),
            booked_again("every repo", later),
        ))
    );
    let calls = m.calls();
    // sam's limit holds for the rest of sam's repos: not asked again.
    assert!(
        !calls.iter().any(|c| c.contains("repos/sam/c")),
        "{calls:#?}"
    );
    let runs: Vec<String> = calls
        .iter()
        .filter_map(|c| c.strip_prefix("renovate "))
        .map(|r| r.split(" token=").next().unwrap().to_string())
        .collect();
    // Another owner's installation has a limit of its own.
    assert_eq!(runs, [filter("sam/a"), filter("lee/d")]);
    assert_eq!(
        m.booked(),
        serde_json::json!({"sam/b": at, "sam/c": at, "": later})
    );

    // A run that failed still fails the pass, with what was held off booked.
    let m = Machine::new(Some(&["sam"]));
    m.set("sam-repos", "sam/a\nsam/b\n");
    m.set("sam_b-limited", &at.to_string());
    m.set("renovate-fails", "sam/a");
    let (code, out, err) = m.run(&[]);
    assert_eq!(code, Some(1));
    assert!(out.starts_with(&booked_again("sam/b", at)), "{out}");
    assert_eq!(err, "sam/a: Renovate's run failed\n");
    assert_eq!(m.booked(), serde_json::json!({"sam/b": at}));

    // The full pass it starts once due uses its booking up.
    let m = Machine::new(Some(&["sam"]));
    m.set("sam-repos", "sam/a\n");
    fs::write(m.path("wake.json"), r#"{"": 1}"#).unwrap();
    assert_eq!(m.run(&[]), ok(""));
    assert_eq!(m.booked(), serde_json::json!({}));
}

#[test]
fn a_wait_it_cant_book_is_a_failure() {
    let m = Machine::new(Some(&["sam"]));
    m.set("sam_r-limited", &in_an_hour().to_string());
    fs::create_dir(m.path("wake.json")).unwrap();
    let (code, _, err) = m.run(&["sam/r"]);
    assert_eq!(code, Some(1));
    assert!(err.starts_with("renovate-run: "), "{err}");
}

#[test]
fn a_run_renovate_stops_on_a_spent_allowance_is_booked_again_not_failed() {
    // Renovate meets the limit mid-run and fails; GitHub's rate_limit says
    // the owner's installation is spent: booked for when it lifts.
    let at = in_an_hour();
    let met = |what: &str| {
        format!(
            "{}: Renovate's run met GitHub's rate limit\n",
            booked_again(what, at)
        )
    };
    // The owner as GitHub spells it, whose installation the token is for.
    let m = Machine::new(Some(&["sam"]));
    m.set("Sam-spent", &at.to_string());
    m.set("renovate-fails", "Sam/R");
    assert_eq!(m.run(&["sam/r"]), ok(&met("sam/r")));
    assert_eq!(m.booked(), serde_json::json!({"sam/r": at}));
    let calls = m.calls();
    assert!(
        calls
            .iter()
            .any(|c| c.starts_with("gh api -i -X GET rate_limit") && c.contains("owner=Sam")),
        "{calls:#?}"
    );

    // In a full run, the rest of that owner's repos are booked with it,
    // and another owner's installation has an allowance of its own.
    let m = Machine::new(Some(&["sam", "kim"]));
    m.set("sam-repos", "sam/a\nsam/b\n");
    m.set("kim-repos", "kim/c\n");
    m.set("sam-spent", &at.to_string());
    m.set("renovate-fails", "sam/a");
    assert_eq!(
        m.run(&[]),
        ok(&format!("{}{}\n", met("sam/a"), booked_again("sam/b", at)))
    );
    assert_eq!(m.runs(), [filter("sam/a"), filter("kim/c")]);
    assert_eq!(m.booked(), serde_json::json!({"sam/a": at, "sam/b": at}));

    // As the user, the one run over everything, by the login's allowance.
    let m = Machine::new(None);
    m.set("user-spent", &at.to_string());
    m.set("renovate-fails", "");
    assert_eq!(m.run(&[]), ok(&met("every repo")));
    assert_eq!(m.booked(), serde_json::json!({"": at}));

    // With allowance left, a run that fails failed.
    let m = Machine::new(Some(&["sam"]));
    m.set("renovate-fails", "Sam/R");
    assert_eq!(m.run(&["sam/r"]), (Some(1), String::new(), String::new()));
    assert_eq!(m.booked(), serde_json::json!({}));
}
