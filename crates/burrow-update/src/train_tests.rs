//! HOST-update's PR on its own: real git, with a bare repo for GitHub's copy
//! and a stand-in for runuser, and a GitHub that answers from a script.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use burrow_github::fake::{Fake, ok};
use burrow_github::{Failed, Response};
use serde_json::{Value, json};

use super::*;

const BRANCH: &str = "box-update";

/// The stand-in for runuser, written once for every test: a file being
/// written while another test starts a process is open for writing in that
/// process until it execs, and running the file then fails with "Text file
/// busy". It runs what it's given as it is, as the machine's user would,
/// after a test's hook for a push from the clone WORK: WORK.before-push, a
/// script it runs once.
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
if [ "$4 $5 $7" = "git -C push" ] && [ -e "$6.before-push" ]; then
  sh "$6.before-push" && rm "$6.before-push"
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

fn train() -> Train {
    Train {
        repo: "sam/homelab".into(),
        branch: BRANCH.into(),
        settled: "review-settled".into(),
        context: "conary/train".into(),
        steward: "sams-steward".into(),
    }
}

/// A machine's clone of its flake at main, GitHub's copy beside it as a
/// bare repo with one commit on main, and HOST-update's state, with nothing
/// proposed yet.
struct Machine {
    dir: tempfile::TempDir,
    cfg: Config,
}

impl Machine {
    fn new() -> Machine {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().to_path_buf();
        let home = d.join("home");
        fs::create_dir(&home).unwrap();
        fs::write(
            home.join(".gitconfig"),
            "[user]\n\tname = test\n\temail = test@example.org\n[init]\n\tdefaultBranch = main\n[advice]\n\tdetachedHead = false\n",
        )
        .unwrap();
        fs::create_dir(d.join("state")).unwrap();
        let cfg = Config {
            update: "box-update".into(),
            host: "box".into(),
            state: d.join("state"),
            work: d.join("work"),
            user: "sam".into(),
            home: home.to_str().unwrap().into(),
            uid: 1000,
            runuser: runuser(),
            train: Some(train()),
        };
        let m = Machine { dir, cfg };
        let d = m.dir.path();
        m.git(d, &["init", "-q", "--bare", "origin.git"]);
        m.git(d, &["clone", "-q", "origin.git", "work"]);
        m.commit("flake.lock", "v1");
        m.work(&["push", "-q", "origin", "HEAD:main"]);
        m
    }

    fn t(&self) -> &Train {
        self.cfg.train.as_ref().unwrap()
    }

    fn path(&self, p: &str) -> PathBuf {
        self.dir.path().join(p)
    }

    /// git in `at`, as the test, with the machine's user's config.
    fn git(&self, at: &Path, args: &[&str]) -> String {
        let out = self.git_at(at).args(args).output().unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim_end().to_string()
    }

    fn git_at(&self, at: &Path) -> Command {
        let mut c = Command::new("git");
        c.arg("-C")
            .arg(at)
            .env("HOME", &self.cfg.home)
            .env("GIT_CONFIG_NOSYSTEM", "1");
        c
    }

    fn work(&self, args: &[&str]) -> String {
        self.git(&self.cfg.work, args)
    }

    fn origin(&self, args: &[&str]) -> String {
        self.git(&self.path("origin.git"), args)
    }

    /// `file` holding `text`, committed in the clone with HOST-update's word
    /// on what it passed.
    fn commit(&self, file: &str, text: &str) {
        fs::write(self.cfg.work.join(file), format!("{text}\n")).unwrap();
        self.work(&["add", file]);
        self.work(&[
            "commit",
            "-qm",
            &format!("{file}: {text}"),
            "-m",
            "Pushed by box-update after the build, the VM tests and the health check passed.",
        ]);
    }

    /// `file` holding `text`, committed on GitHub's `branch` from a clone of
    /// its own (`by`), and pushed there: its new head.
    fn pushed(&self, by: &str, branch: &str, file: &str, text: &str) -> String {
        let other = self.path(by);
        let _ = fs::remove_dir_all(&other);
        let origin = self.path("origin.git");
        self.git(
            self.dir.path(),
            &["clone", "-q", "-b", branch, origin.to_str().unwrap(), by],
        );
        fs::write(other.join(file), format!("{text}\n")).unwrap();
        self.git(&other, &["add", file]);
        self.git(&other, &["commit", "-qm", &format!("{file}: {text}")]);
        self.git(
            &other,
            &["push", "-q", "--force", "origin", &format!("HEAD:{branch}")],
        );
        self.git(&other, &["rev-parse", "HEAD"])
    }

    /// main moving on GitHub, as another PR's landing would.
    fn landed(&self, file: &str, text: &str) {
        self.pushed("other", "main", file, text);
    }

    /// A head of its own on its PR, #7, pushed onto main by a run before
    /// this one from a clone since gone: GitHub's branch has it, and this
    /// clone doesn't until it's fetched.
    fn pushed_before(&self, text: &str) -> Pr {
        self.origin(&["branch", "-f", BRANCH, "main"]);
        let head = self.pushed("before", BRANCH, "flake.lock", text);
        self.propose_was(&head);
        Pr { number: 7, head }
    }

    /// What HOST-update remembers it pushed last.
    fn propose_was(&self, head: &str) {
        fs::write(self.cfg.state.join("proposed"), format!("{head}\n")).unwrap();
    }

    /// The clone as update.sh leaves it before riding: at GitHub's main.
    fn at_main(&self) -> String {
        self.work(&["fetch", "-q", "origin", "main"]);
        self.work(&["checkout", "-q", "-B", "update", "origin/main"]);
        self.work(&["reset", "-q", "--hard", "origin/main"]);
        self.head()
    }

    fn head(&self) -> String {
        self.work(&["rev-parse", "HEAD"])
    }

    /// GitHub's copy of its PR's branch.
    fn branch(&self) -> String {
        self.origin(&["rev-parse", &format!("refs/heads/{BRANCH}")])
    }

    fn main(&self) -> String {
        self.origin(&["rev-parse", "refs/heads/main"])
    }

    fn proposed(&self) -> Option<String> {
        fs::read_to_string(self.cfg.state.join("proposed"))
            .ok()
            .map(|s| s.trim_end().to_string())
    }

    /// Whether the clone has `commit`.
    fn has(&self, commit: &str) -> bool {
        self.git_at(&self.cfg.work)
            .args(["cat-file", "-e", &format!("{commit}^{{tree}}")])
            .output()
            .unwrap()
            .status
            .success()
    }

    /// Whether the clone is as it was: at `at`, with no merge half done and
    /// nothing changed.
    fn left_at(&self, at: &str) -> bool {
        self.head() == at
            && !self.cfg.work.join(".git/MERGE_HEAD").exists()
            && self.work(&["status", "--porcelain"]).is_empty()
    }
}

/// GitHub's list of open PRs: each one's number, head and head's repo.
fn listed(prs: &[(u64, String, Value)]) -> Value {
    prs.iter()
        .map(|(n, sha, repo)| {
            let repo = if repo.is_null() {
                Value::Null
            } else {
                json!({"full_name": repo})
            };
            json!({"number": n, "head": {"sha": sha, "ref": BRANCH, "repo": repo}})
        })
        .collect()
}

fn sha(c: char) -> String {
    c.to_string().repeat(40)
}

fn down() -> Result<Response, Failed> {
    Err(Failed {
        why: "gh: error connecting to api.github.com".into(),
        transient: true,
    })
}

/// A check run, as GraphQL has it.
fn run(status: &str, conclusion: Option<&str>) -> Value {
    json!({"__typename": "CheckRun", "status": status, "conclusion": conclusion})
}

/// A status, posted by the steward's App's bot.
fn status(context: &str, state: &str) -> Value {
    status_by(context, state, "sams-steward", "Bot")
}

/// A status posted by `by`, a `kind` (Bot or User).
fn status_by(context: &str, state: &str, by: &str, kind: &str) -> Value {
    json!({"__typename": "StatusContext", "context": context, "state": state,
           "creator": {"login": by, "__typename": kind}})
}

/// A check suite: kept apart from the rest by `checked`.
fn suite(conclusion: &str) -> Value {
    json!({"__typename": "CheckSuite", "conclusion": conclusion})
}

/// GraphQL's answer for a PR whose head is `head`, with these checks; with
/// `more`, they have a page more.
fn checked(head: &str, checks: &[Value], more: bool) -> Value {
    let (suites, contexts): (Vec<&Value>, Vec<&Value>) =
        checks.iter().partition(|c| c["__typename"] == "CheckSuite");
    let suites: Vec<Value> = suites
        .into_iter()
        .map(|s| json!({"conclusion": s["conclusion"]}))
        .collect();
    json!({"data": {"repository": {"pullRequest": {"headRefOid": head, "commits": {"nodes": [{"commit": {
        "statusCheckRollup": {"contexts": {"pageInfo": {"hasNextPage": more}, "nodes": contexts}},
        "checkSuites": {"pageInfo": {"hasNextPage": false}, "nodes": suites}}}]}}}}})
}

/// A GitHub that says `head`'s checks are these.
fn github(head: &str, checks: &[Value]) -> Fake {
    let mut gh = Fake::default();
    gh.on(
        "POST",
        "graphql",
        vec![ok(200, checked(head, checks, false), &[])],
    );
    gh
}

#[test]
fn a_pr_is_read_as_update_sh_hands_it_over() {
    let a = sha('a');
    let pr = Pr::parse(&format!("7 {a}")).unwrap();
    assert_eq!(
        pr,
        Pr {
            number: 7,
            head: a.clone()
        }
    );
    assert_eq!(pr.to_string(), format!("7 {a}"));
    assert!(Pr::parse(&format!("7 {}", "b".repeat(64))).is_some());
    for wrong in [
        String::new(),
        "7".into(),
        format!("7  {a}"),
        format!("x {a}"),
        format!("-7 {a}"),
        format!(" {a}"),
        format!("7 {}", "A".repeat(40)),
        "7 abc".into(),
        format!("7 -{}", "a".repeat(39)),
        format!("7 {a} 8"),
        format!("99999999999999999999999 {a}"),
    ] {
        assert_eq!(Pr::parse(&wrong), None, "{wrong:?}");
    }
}

#[test]
fn its_pr_is_its_own_branchs_open_one() {
    let t = train();
    let on = |answer| {
        let mut gh = Fake::default();
        gh.on("GET", "repos/sam/homelab/pulls?", vec![answer]);
        gh
    };
    let mut gh = on(ok(200, json!([]), &[]));
    assert_eq!(open(&t, &mut gh), Ok(None));
    assert_eq!(
        gh.asked[0].1,
        "repos/sam/homelab/pulls?state=open&base=main&head=sam:box-update"
    );
    let ours = (7, sha('a'), json!("sam/homelab"));
    let pr = Some(Pr {
        number: 7,
        head: sha('a'),
    });
    let mut gh = on(ok(200, listed(std::slice::from_ref(&ours)), &[]));
    assert_eq!(open(&t, &mut gh), Ok(pr.clone()));
    // A fork of the owner's own, under another name, has a head of the same
    // name: never its own, newer than its own or alone. Nor is a head whose
    // repo is gone. GitHub's names go by any case.
    let fork = (9, sha('b'), json!("sam/homelab-fork"));
    let gone = (9, sha('b'), Value::Null);
    let cased = (7, sha('a'), json!("Sam/HomeLab"));
    for (prs, said) in [
        (vec![fork.clone(), ours], pr.clone()),
        (vec![fork], None),
        (vec![gone, cased], pr),
    ] {
        assert_eq!(open(&t, &mut on(ok(200, listed(&prs), &[]))), Ok(said));
    }
    // GitHub down, or an answer that isn't a list of PRs: a failure, never
    // "none open".
    let e = open(&t, &mut on(down())).unwrap_err();
    assert!(e.starts_with("GET repos/sam/homelab/pulls?"), "{e}");
    let e = open(&t, &mut on(ok(200, json!({"message": "x"}), &[]))).unwrap_err();
    assert!(e.ends_with("not a list of PRs"), "{e}");
    let odd = (7, "abc".to_string(), json!("sam/homelab"));
    let e = open(&t, &mut on(ok(200, listed(&[odd]), &[]))).unwrap_err();
    assert!(e.ends_with("a PR that isn't one (7 abc)"), "{e}");
}

#[test]
fn its_pr_says_what_the_commit_passed_and_that_its_live() {
    let m = Machine::new();
    m.commit("flake.lock", "v2");
    let (title, body) = said(&m.cfg).unwrap();
    assert_eq!(title, "flake.lock: v2");
    assert_eq!(
        body,
        "Pushed by box-update after the build, the VM tests and the health check passed.\n\n\
         It is live on box. The steward settles this PR and the merge train lands it; until then \
         box-update brings in nothing newer, unless a check fails on it: then a fresh bump replaces it."
    );
}

#[test]
fn proposing_puts_head_up_as_a_new_pr() {
    let m = Machine::new();
    // An old PR's head, never landed: HEAD goes over it.
    m.commit("flake.lock", "old");
    m.work(&["push", "-q", "origin", &format!("HEAD:refs/heads/{BRANCH}")]);
    m.work(&["reset", "-q", "--hard", "HEAD~1"]);
    m.commit("flake.lock", "v2");
    let mut gh = Fake::default();
    let url = "https://github.com/sam/homelab/pull/7";
    gh.on(
        "POST",
        "repos/sam/homelab/pulls",
        vec![ok(201, json!({"html_url": url}), &[])],
    );
    assert_eq!(propose(&m.cfg, m.t(), &mut gh).as_deref(), Ok(url));
    let head = m.head();
    assert_eq!(m.branch(), head);
    assert_eq!(m.proposed().as_deref(), Some(head.as_str()));
    assert_ne!(m.main(), head);
    let (title, body) = said(&m.cfg).unwrap();
    assert_eq!(
        gh.asked,
        vec![(
            "POST".into(),
            "repos/sam/homelab/pulls".into(),
            Some(json!({"title": title, "head": BRANCH, "base": "main", "body": body})),
            None
        )]
    );

    // GitHub refusing it: a failure, with the branch pushed and remembered,
    // for the next run to find none open and open it.
    m.commit("flake.lock", "v3");
    let mut gh = Fake::default();
    let no = json!({"message": "Validation Failed"});
    gh.on("POST", "repos/sam/homelab/pulls", vec![ok(422, no, &[])]);
    let e = propose(&m.cfg, m.t(), &mut gh).unwrap_err();
    assert!(e.ends_with("HTTP 422: Validation Failed"), "{e}");
    assert_eq!(m.branch(), m.head());
    assert_eq!(m.proposed(), Some(m.head()));

    // A push that's refused: a failure, nothing remembered and nothing asked.
    let before = m.proposed();
    m.commit("flake.lock", "v4");
    fs::rename(m.path("origin.git"), m.path("gone.git")).unwrap();
    let mut gh = Fake::default();
    assert!(propose(&m.cfg, m.t(), &mut gh).is_err());
    assert_eq!(m.proposed(), before);
    assert!(gh.asked.is_empty());
}

#[test]
fn riding_is_main_with_its_own_head_on_top() {
    // Main where the PR left it: HEAD is the PR's head.
    let m = Machine::new();
    let pr = m.pushed_before("v2");
    m.at_main();
    assert_eq!(ride(&m.cfg, m.t(), &pr), Ok(()));
    assert_eq!(m.head(), pr.head);

    // Main moved on: main with the PR on top.
    m.landed("README", "new");
    let main = m.at_main();
    assert_eq!(ride(&m.cfg, m.t(), &pr), Ok(()));
    m.work(&["merge-base", "--is-ancestor", &main, "HEAD"]);
    m.work(&["merge-base", "--is-ancestor", &pr.head, "HEAD"]);
    let read = |f: &str| fs::read_to_string(m.cfg.work.join(f)).unwrap();
    assert_eq!(
        (read("flake.lock"), read("README")),
        ("v2\n".into(), "new\n".into())
    );
    assert_eq!(m.work(&["log", "-1", "--format=%s"]), "Ride #7 on main");
}

#[test]
fn it_rides_only_its_own_head_as_it_was_listed_and_only_onto_main() {
    // Someone else's head on it (a fix job's): no riding, HEAD still main.
    let m = Machine::new();
    m.pushed_before("v2");
    let fix = Pr {
        number: 7,
        head: m.pushed("fix", BRANCH, "flake.lock", "v3"),
    };
    let main = m.at_main();
    assert_eq!(
        ride(&m.cfg, m.t(), &fix),
        Err(format!(
            "its PR #7 has a head it didn't push ({}); holding until that lands",
            fix.head
        ))
    );
    assert!(m.left_at(&main));
    // Nothing proposed at all: the same.
    fs::remove_file(m.cfg.state.join("proposed")).unwrap();
    let e = ride(&m.cfg, m.t(), &fix).unwrap_err();
    assert!(e.contains("a head it didn't push"), "{e}");

    // GitHub's branch moved since it was listed: no riding what wasn't read.
    let m = Machine::new();
    let listed = m.pushed_before("v2");
    m.pushed("fix", BRANCH, "flake.lock", "v3");
    let main = m.at_main();
    assert_eq!(
        ride(&m.cfg, m.t(), &listed),
        Err(format!("couldn't fetch its PR #7's head ({})", listed.head))
    );
    assert!(m.left_at(&main));

    // A PR that no longer merges onto main: no riding, and no merge left
    // half done.
    let m = Machine::new();
    let pr = m.pushed_before("v2");
    m.landed("flake.lock", "other");
    let main = m.at_main();
    assert_eq!(
        ride(&m.cfg, m.t(), &pr),
        Err("its PR #7 doesn't merge onto main; holding until it's fixed".into())
    );
    assert!(m.left_at(&main));
}

#[test]
fn red_is_ci_as_the_steward_reads_it_on_its_own_head() {
    let m = Machine::new();
    let pr = m.pushed_before("v2");
    // Green, pending, rerunning, the steward's own status, the train's error
    // (a merge GitHub refused), its failure from anyone else: not red, so
    // it's ridden as before.
    for green in [
        vec![],
        vec![run("IN_PROGRESS", None)],
        vec![
            run("COMPLETED", Some("SUCCESS")),
            run("COMPLETED", Some("SKIPPED")),
            run("COMPLETED", Some("NEUTRAL")),
        ],
        vec![
            status("ci/proof", "PENDING"),
            status("ci/proof", "EXPECTED"),
            status("conary/train", "SUCCESS"),
        ],
        vec![status("review-settled", "FAILURE")],
        vec![status("conary/train", "ERROR")],
        vec![status_by("conary/train", "FAILURE", "mallory", "User")],
        vec![status_by("conary/train", "FAILURE", "sams-steward", "User")],
        vec![status_by("conary/train", "FAILURE", "other-app", "Bot")],
        vec![suite("SUCCESS"), run("COMPLETED", Some("SUCCESS"))],
    ] {
        let said = red(&m.cfg, m.t(), &mut github(&pr.head, &green), &pr);
        assert_eq!(said, Ok(false), "{green:?}");
    }
    assert!(!m.has(&pr.head), "fetched a head that isn't red");
    // Red: a check on its own head failed, a workflow failed to start, a
    // status failed, or the train found the head red alone (its failure,
    // from the steward's App). It's fetched, for `replaces`.
    for (i, failed) in [
        vec![run("COMPLETED", Some("FAILURE"))],
        vec![run("COMPLETED", Some("TIMED_OUT"))],
        vec![run("COMPLETED", None)],
        vec![
            run("COMPLETED", Some("CANCELLED")),
            run("COMPLETED", Some("SUCCESS")),
        ],
        vec![status("conary/train", "FAILURE")],
        vec![status("ci/proof", "ERROR")],
        vec![status_by("ci/proof", "FAILURE", "mallory", "User")],
        vec![suite("STARTUP_FAILURE")],
        vec![
            suite("STARTUP_FAILURE"),
            suite("SUCCESS"),
            run("COMPLETED", Some("SUCCESS")),
        ],
    ]
    .into_iter()
    .enumerate()
    {
        let said = red(&m.cfg, m.t(), &mut github(&pr.head, &failed), &pr);
        assert_eq!(said, Ok(true), "{failed:?}");
        if i == 0 {
            assert!(m.has(&pr.head), "didn't fetch the red head");
        }
    }
    // What it asked, of the account it's about.
    let mut gh = github(&pr.head, &[]);
    red(&m.cfg, m.t(), &mut gh, &pr).unwrap();
    assert_eq!(
        gh.asked[0].2,
        Some(json!({"query": CHECKS, "variables": {"owner": "sam", "name": "homelab", "n": 7}}))
    );
    // Over 100 checks, one red: not red, as the steward reads it.
    let mut gh = Fake::default();
    let more = checked(&pr.head, &[run("COMPLETED", Some("FAILURE"))], true);
    gh.on("POST", "graphql", vec![ok(200, more, &[])]);
    assert_eq!(red(&m.cfg, m.t(), &mut gh, &pr), Ok(false));
}

#[test]
fn red_says_nothing_of_a_head_it_didnt_push_and_fails_when_it_cant_tell() {
    let m = Machine::new();
    let pr = m.pushed_before("v2");
    let failed = [run("COMPLETED", Some("FAILURE"))];
    // Someone else's head: not red, and GitHub isn't asked.
    let theirs = Pr {
        number: 7,
        head: m.main(),
    };
    let mut gh = github(&theirs.head, &failed);
    assert_eq!(red(&m.cfg, m.t(), &mut gh, &theirs), Ok(false));
    assert!(gh.asked.is_empty());
    // GitHub down: a failure, never "not red".
    let mut gh = Fake::default();
    gh.on("POST", "graphql", vec![down()]);
    assert_eq!(
        red(&m.cfg, m.t(), &mut gh, &pr),
        Err("GitHub didn't say how its PR #7's checks stand".into())
    );
    // GitHub's head moved on after the listing: a failure.
    let now = m.pushed_before("v3");
    m.propose_was(&pr.head);
    let mut gh = github(&now.head, &failed);
    assert_eq!(
        red(&m.cfg, m.t(), &mut gh, &pr),
        Err(format!("its PR #7's head isn't {} any more", pr.head))
    );
    // Red as GitHub read it, but the branch moved before it could be
    // fetched: a failure, and nothing to tell it apart from.
    let mut gh = github(&pr.head, &failed);
    assert_eq!(
        red(&m.cfg, m.t(), &mut gh, &pr),
        Err(format!("couldn't fetch its PR #7's head ({})", pr.head))
    );
}

#[test]
fn a_fresh_bump_replaces_a_red_head_only_whole_and_other_than_it() {
    let m = Machine::new();
    let pr = m.pushed_before("v2");
    // A head it never fetched can't be told apart from a fresh bump.
    let main = m.at_main();
    m.commit("flake.lock", "v2");
    let e = replaces(&m.cfg, &pr, &main, &[]).unwrap_err();
    assert!(
        e.starts_with("a fresh bump can't be told apart from it"),
        "{e}"
    );
    let mut gh = github(&pr.head, &[run("COMPLETED", Some("FAILURE"))]);
    assert_eq!(red(&m.cfg, m.t(), &mut gh, &pr), Ok(true));
    // Not the red head's very tree (nothing moved), nor main itself with
    // nothing new on it (all its inputs failed, or none could be fetched, or
    // none moved), nor one short of an input; once main moved, a whole one
    // does.
    assert_eq!(
        replaces(&m.cfg, &pr, &main, &[]),
        Err("a fresh bump is that same tree".into())
    );
    m.landed("README", "new");
    let main = m.at_main();
    assert_eq!(
        replaces(&m.cfg, &pr, &main, &[]),
        Err("a fresh bump has nothing new on main".into())
    );
    let every = ["claude-code".to_string(), "nixpkgs".to_string()];
    assert_eq!(
        replaces(&m.cfg, &pr, &main, &every),
        Err("a fresh bump is short of claude-code nixpkgs".into())
    );
    m.commit("flake.lock", "v3");
    assert_eq!(
        replaces(&m.cfg, &pr, &main, &every[..1]),
        Err("a fresh bump is short of claude-code".into())
    );
    assert_eq!(replaces(&m.cfg, &pr, &main, &[]), Ok(()));
}

/// A machine with its red PR #7 open, and a fresh bump in its clone.
fn red_with_a_fresh_bump() -> (Machine, Pr) {
    let m = Machine::new();
    let pr = m.pushed_before("v2");
    m.at_main();
    m.commit("flake.lock", "v3");
    (m, pr)
}

#[test]
fn replacing_puts_the_fresh_bump_up_as_the_same_prs_new_head() {
    let (m, pr) = red_with_a_fresh_bump();
    let main = m.main();
    let mut gh = github(&pr.head, &[run("COMPLETED", Some("FAILURE"))]);
    let url = "https://github.com/sam/homelab/pull/7";
    gh.on(
        "PATCH",
        "repos/sam/homelab/pulls/7",
        vec![ok(200, json!({"html_url": url}), &[])],
    );
    assert_eq!(
        replace(&m.cfg, m.t(), &mut gh, &pr),
        Ok(format!("{url}, in place of its red head {}", &pr.head[..7]))
    );
    let fresh = m.head();
    assert_eq!(m.branch(), fresh);
    assert_eq!(m.proposed(), Some(fresh));
    let (title, body) = said(&m.cfg).unwrap();
    let patched = gh.asked("PATCH");
    assert_eq!(patched.len(), 1);
    assert_eq!(patched[0].2, Some(json!({"title": title, "body": body})));
    assert_eq!(gh.asked("POST").len(), 1, "opened a new PR");
    assert_eq!(m.main(), main);

    // GitHub not taking the new title: it says so, and the head stays new.
    let (m, pr) = red_with_a_fresh_bump();
    let mut gh = github(&pr.head, &[run("COMPLETED", Some("FAILURE"))]);
    gh.on(
        "PATCH",
        "repos/sam/homelab/pulls/7",
        vec![ok(403, json!({"message": "no"}), &[])],
    );
    assert_eq!(
        replace(&m.cfg, m.t(), &mut gh, &pr),
        Ok(format!(
            "https://github.com/sam/homelab/pull/7 (its title still says the red head's: GitHub didn't take the new one), in place of its red head {}",
            &pr.head[..7]
        ))
    );
    assert_eq!(m.branch(), m.head());
}

#[test]
fn replacing_leaves_a_head_gone_green_or_fixed_or_unread() {
    // Green again since (a rerun): left as it was, to ride.
    let (m, pr) = red_with_a_fresh_bump();
    let mut gh = github(&pr.head, &[run("IN_PROGRESS", None)]);
    assert_eq!(
        replace(&m.cfg, m.t(), &mut gh, &pr),
        Err("its PR #7 isn't red any more, so it stays as it is".into())
    );
    assert_eq!(
        (m.branch(), m.proposed()),
        (pr.head.clone(), Some(pr.head.clone()))
    );

    // A fix pushed between its look and its push: the push is refused, and
    // the fix stays.
    let (m, pr) = red_with_a_fresh_bump();
    let hook = format!(
        "export HOME='{home}' GIT_CONFIG_NOSYSTEM=1\n\
         git clone -q -b {BRANCH} '{origin}' '{fix}' 2>/dev/null\n\
         echo fixed >'{fix}/flake.lock'\n\
         git -C '{fix}' commit -qam 'fix it'\n\
         git -C '{fix}' push -q origin HEAD:{BRANCH}\n",
        home = m.cfg.home,
        origin = m.path("origin.git").display(),
        fix = m.path("fix").display(),
    );
    fs::write(m.path("work.before-push"), hook).unwrap();
    let mut gh = github(&pr.head, &[run("COMPLETED", Some("FAILURE"))]);
    assert_eq!(
        replace(&m.cfg, m.t(), &mut gh, &pr),
        Err(format!(
            "its PR #7 moved on from {} while this ran, so it stays as it is",
            pr.head
        ))
    );
    assert!(!m.path("work.before-push").exists(), "the fix never came");
    assert_eq!(
        m.origin(&["show", &format!("refs/heads/{BRANCH}:flake.lock")]),
        "fixed"
    );
    assert_eq!(m.proposed(), Some(pr.head.clone()));
    assert!(
        gh.asked("PATCH").is_empty(),
        "retitled a PR it didn't push to"
    );

    // GitHub not saying how its checks stand: left as it was.
    let (m, pr) = red_with_a_fresh_bump();
    let mut gh = Fake::default();
    gh.on("POST", "graphql", vec![down()]);
    assert_eq!(
        replace(&m.cfg, m.t(), &mut gh, &pr),
        Err("GitHub didn't say how its PR #7's checks stand; its PR #7 stays as it is".into())
    );
    assert_eq!(m.branch(), pr.head);
}
