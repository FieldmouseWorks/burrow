//! A PR's own change, fingerprinted from a real repo standing in for
//! GitHub: the same across a merge of its base that leaves its lines
//! alone, and another once anything of its own moves.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::*;
use crate::testing::{config, put, sh};

/// Twenty numbered lines, line `at` (1-based) said as `said`.
fn lines(at: usize, said: &str) -> String {
    (1..=20)
        .map(|i| {
            if i == at {
                format!("{said}\n")
            } else {
                format!("line {i}\n")
            }
        })
        .collect()
}

/// A repo: main with a.txt and b.txt, and a PR's branch `feat` off it that
/// changes a.txt's 15th line and adds c.txt. Main moves on since: it adds
/// two lines atop a.txt (moving where the PR's hunk sits), changes its 12th
/// (in the PR's hunk's context), and adds d.txt.
struct Rig {
    _d: tempfile::TempDir,
    work: PathBuf,
}

impl Rig {
    fn new() -> Rig {
        let d = tempfile::tempdir().unwrap();
        let work = d.path().join("work");
        fs::create_dir_all(&work).unwrap();
        let w = &work;
        sh(w, &["init", "-q", "-b", "main"]);
        put(w, "a.txt", &lines(0, ""));
        put(w, "b.txt", "b\n");
        sh(w, &["add", "-A"]);
        sh(w, &["commit", "-qm", "base"]);
        sh(w, &["checkout", "-qb", "feat"]);
        put(w, "a.txt", &lines(15, "the PR's line"));
        put(w, "c.txt", "new\n");
        sh(w, &["add", "-A"]);
        sh(w, &["commit", "-qm", "the PR"]);
        sh(w, &["checkout", "-q", "main"]);
        put(
            w,
            "a.txt",
            &format!("top 1\ntop 2\n{}", lines(12, "main's line")),
        );
        put(w, "d.txt", "main's file\n");
        sh(w, &["add", "-A"]);
        sh(w, &["commit", "-qm", "main moves on"]);
        sh(w, &["checkout", "-q", "feat"]);
        Rig { _d: d, work }
    }

    fn w(&self) -> &Path {
        &self.work
    }

    fn head(&self) -> String {
        sh(self.w(), &["rev-parse", "HEAD"]).trim().to_string()
    }

    /// The fingerprint of feat's head's own change on main.
    fn change(&self) -> String {
        of(self.w(), "refs/heads/main", &self.head()).unwrap()
    }

    fn commit(&self, path: &str, text: &str, msg: &str) {
        put(self.w(), path, text);
        sh(self.w(), &["add", "-A"]);
        sh(self.w(), &["commit", "-qm", msg]);
    }
}

#[test]
fn a_merge_of_the_base_that_leaves_the_prs_lines_alone_keeps_it() {
    let r = Rig::new();
    let before = r.change();
    let reviewed = r.head();
    sh(r.w(), &["merge", "-q", "--no-edit", "main"]);
    assert_ne!(r.head(), reviewed);
    // main's own lines moved the PR's hunk's context and its blobs, and
    // brought a file of its own: none of that is the PR's.
    assert_eq!(r.change(), before);
    // So does a rebase onto main, which no merge commit shows.
    let r = Rig::new();
    sh(r.w(), &["rebase", "-q", "main"]);
    assert_eq!(r.change(), before);
}

#[test]
fn anything_of_the_prs_own_that_moves_changes_it() {
    let r = Rig::new();
    let before = r.change();
    // A new commit.
    r.commit("a.txt", &lines(15, "the PR's line, fixed"), "fix");
    assert_ne!(r.change(), before);
    // A conflict's resolution that edits the PR's line: main changed the
    // same line, and the merge keeps a mix of both.
    let r = Rig::new();
    sh(r.w(), &["checkout", "-q", "main"]);
    r.commit("a.txt", &lines(15, "main's take"), "main's take");
    sh(r.w(), &["checkout", "-q", "feat"]);
    let merged = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .arg("-C")
        .arg(r.w())
        .args(["merge", "-q", "--no-edit", "main"])
        .output()
        .unwrap();
    assert!(!merged.status.success(), "the merge conflicts");
    r.commit(
        "a.txt",
        &lines(15, "the PR's line and main's take"),
        "resolved",
    );
    assert_ne!(r.change(), before);
    // The same lines in another file, or a mode change alone.
    let r = Rig::new();
    sh(r.w(), &["mv", "c.txt", "e.txt"]);
    sh(r.w(), &["commit", "-qm", "renamed"]);
    assert_ne!(r.change(), before);
    let r = Rig::new();
    sh(r.w(), &["update-index", "--chmod=+x", "c.txt"]);
    sh(r.w(), &["commit", "-qm", "executable"]);
    assert_ne!(r.change(), before);
}

#[test]
fn it_keeps_what_the_pr_says_and_drops_where_it_sits() {
    let diff = "diff --git a/a.txt b/a.txt\nindex 1111111..2222222 100644\n--- a/a.txt\n+++ b/a.txt\n@@ -15 +15 @@\n-line 15\n+the PR's line\n";
    let moved = "diff --git a/a.txt b/a.txt\nindex 3333333..4444444 100644\n--- a/a.txt\n+++ b/a.txt\n@@ -17,2 +17,2 @@ fn f\n context\n-line 15\n+the PR's line\n";
    assert_eq!(fingerprint(diff.as_bytes()), fingerprint(moved.as_bytes()));
    assert_eq!(fingerprint(diff.as_bytes()).len(), 64);
    for other in [
        diff.replace("+the PR's line", "+another line"),
        diff.replace("-line 15", "-line 16"),
        diff.replace("a/a.txt b/a.txt", "a/z.txt b/z.txt"),
        // The order of what it adds is its own too.
        diff.replace(
            "@@ -15 +15 @@\n-line 15\n+the PR's line\n",
            "@@ -15 +15 @@\n+the PR's line\n-line 15\n",
        ),
        // And a newline it leaves off.
        format!("{diff}\\ No newline at end of file\n"),
    ] {
        assert_ne!(
            fingerprint(other.as_bytes()),
            fingerprint(diff.as_bytes()),
            "{other}"
        );
    }
}

#[test]
fn only_a_commits_id_is_diffed() {
    let r = Rig::new();
    assert_eq!(
        of(r.w(), "refs/heads/main", "--output=/tmp/x"),
        Err("\"--output=/tmp/x\" isn't a commit's id".into())
    );
}

#[test]
fn it_is_worked_out_from_the_stewards_mirror() {
    let r = Rig::new();
    let cfg = config(r._d.path());
    let origin = Path::new(&cfg.git).join("sam/r.git");
    fs::create_dir_all(&origin).unwrap();
    sh(&origin, &["init", "-q", "--bare"]);
    let to = origin.to_str().unwrap();
    sh(r.w(), &["push", "-q", to, "main", "HEAD:refs/pull/5/head"]);
    let state = cfg.state_dir.clone();
    let got = change(&cfg, &state, "sam/r", 5, &r.head(), "main").unwrap();
    assert_eq!(got, r.change());
    // A base with no such branch: no fingerprint, and why.
    let gone = change(&cfg, &state, "sam/r", 5, &r.head(), "gone").unwrap_err();
    assert!(
        gone.contains("couldn't find remote ref refs/heads/gone"),
        "{gone}"
    );
}
