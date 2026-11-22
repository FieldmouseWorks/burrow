//! The shared CI's commits job, its own script run with gh stood in for: a PR
//! commit signed by a key GitHub can't match to its committer fails it, and
//! an unsigned or verified one doesn't.

use serde_json::json;

use crate::harness::{Scratch, read, script, step, workflow, write, yaml};

fn job() -> yaml_serde::Value {
    workflow(".github/workflows/flake-check.yml")["jobs"]["commits"].clone()
}

/// Only a PR has commits of its own to check, and reading them needs no more
/// than the tree does.
#[test]
fn it_runs_on_a_prs_commits() {
    let job = job();
    assert_eq!(job["if"], "github.event_name == 'pull_request'");
    assert_eq!(job["permissions"], yaml("contents: read"));
    let s = step(&job, "signatures");
    assert_eq!(s["shell"], "bash");
    assert_eq!(
        s["env"]["BASE"],
        "${{ github.event.pull_request.base.sha }}"
    );
    assert_eq!(
        s["env"]["HEAD"],
        "${{ github.event.pull_request.head.sha }}"
    );
}

/// gh as the job calls it, `gh api [--paginate] PATH --jq EXPR`, answered
/// from a compare response, every call logged.
fn stand_in() -> Scratch {
    let s = Scratch::default();
    s.stub(
        "gh",
        r#"echo "$*" >> "$GH_LOG"
[ -z "$GH_FAIL" ] || exit 1
while [ $# -gt 0 ]; do
  [ "$1" = --jq ] && { exec jq -r "$2" "$COMPARE"; }
  shift
done
cat "$COMPARE"
"#,
    );
    s
}

fn commit(sha: &str, reason: &str) -> serde_json::Value {
    commit_by(sha, reason, "Sam Owner", "sam@example.org")
}

fn commit_by(sha: &str, reason: &str, name: &str, email: &str) -> serde_json::Value {
    json!({"sha": sha, "commit": {"committer": {"name": name, "email": email},
           "verification": {"verified": reason == "valid", "reason": reason}}})
}

/// The step, on a PR with these commits: its exit code, what it printed and
/// gh's calls.
fn run(s: &Scratch, commits: &[serde_json::Value], fail: bool) -> (i32, String, Vec<String>) {
    let compare = s.join("compare.json");
    let body = json!({"merge_base_commit": {"sha": "f0rk"}, "commits": commits});
    write(&compare, &body.to_string());
    let log = s.join("gh.log");
    let _ = std::fs::remove_file(&log);
    let job = job();
    let ran = s.bash(
        script(step(&job, "signatures")),
        s.path(),
        &[
            ("COMPARE", compare.to_str().unwrap()),
            ("GH_LOG", log.to_str().unwrap()),
            ("GH_FAIL", if fail { "1" } else { "" }),
            ("GH_TOKEN", "t"),
            ("GITHUB_REPOSITORY", "sam/r"),
            ("BASE", "ba5e"),
            ("HEAD", "4ead"),
            ("RUNNER_TEMP", s.path().to_str().unwrap()),
        ],
    );
    let calls = if log.exists() {
        read(&log).lines().map(str::to_owned).collect()
    } else {
        Vec::new()
    };
    (ran.code, ran.said(), calls)
}

/// Verified and unsigned commits pass: a commit GitHub made (a squash, a web
/// edit) and one made on a machine that doesn't sign. It asks for exactly
/// the PR's commits: what HEAD has that its merge base with BASE doesn't,
/// every page of them.
#[test]
fn verified_and_unsigned_commits_pass() {
    let s = stand_in();
    let commits = [
        commit("a1", "valid"),
        commit_by("b2", "unsigned", "remi-renovate", "renovate@example.org"),
    ];
    let (code, out, calls) = run(&s, &commits, false);
    assert!(code == 0 && !out.contains("::error"), "{out}");
    assert!(
        !calls.is_empty()
            && calls
                .iter()
                .all(|c| c.contains("repos/sam/r/compare/ba5e...4ead")),
        "{calls:?}"
    );
    assert!(calls.iter().any(|c| c.contains("--paginate")), "{calls:?}");
}

/// The case it's for: a session's own key on a commit named for a person, so
/// GitHub can't find the key on that person's account. Only that commit is
/// named, with GitHub's reason and how to recommit it.
#[test]
fn a_signature_github_cant_match_to_its_committer_fails() {
    let s = stand_in();
    let commits = [
        commit("a1", "valid"),
        commit("c3", "unknown_key"),
        commit("b2", "unsigned"),
    ];
    let (code, out, _) = run(&s, &commits, false);
    assert_eq!(code, 1, "{out}");
    let errors: Vec<&str> = out.lines().filter(|l| l.starts_with("::error")).collect();
    assert!(
        errors.len() == 1
            && errors[0].contains("c3 is signed")
            && errors[0].contains("(unknown_key)"),
        "{out}"
    );
    assert!(errors[0].contains("--reset-author' f0rk"), "{}", errors[0]);
}

/// Any other signature GitHub doesn't verify fails it too.
#[test]
fn every_other_unverified_signature_fails() {
    let s = stand_in();
    for reason in [
        "invalid",
        "no_user",
        "bad_email",
        "unverified_email",
        "expired_key",
        "not_signing_key",
        "malformed_signature",
    ] {
        let (code, out, _) = run(&s, &[commit("d4", reason)], false);
        assert!(
            code == 1 && out.contains(&format!("({reason})")),
            "{reason}: {out}"
        );
    }
}

/// The log names commits, never people: no committer's name or email,
/// whatever the verdict.
#[test]
fn no_names_or_emails_in_the_log() {
    let s = stand_in();
    for reason in [
        "valid",
        "unsigned",
        "unknown_key",
        "invalid",
        "no_user",
        "bad_email",
    ] {
        let (_, out, _) = run(
            &s,
            &[commit_by("e5", reason, "Kim Person", "kim@example.org")],
            false,
        );
        assert!(
            !out.contains("Kim") && !out.contains("kim@example.org"),
            "{reason}: {out}"
        );
    }
}

/// GitHub not answering fails it: no answer is no verdict.
#[test]
fn no_answer_from_github_fails() {
    let s = stand_in();
    let (code, out, _) = run(&s, &[commit("a1", "valid")], true);
    assert_ne!(code, 0, "{out}");
}
