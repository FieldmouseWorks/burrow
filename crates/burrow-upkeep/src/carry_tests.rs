//! Which earlier heads' looks cover a PR's head, worked out in a real repo
//! standing in for GitHub and the steward's mirror.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::json;

use super::*;
use crate::testing::{config, put, sh};
use crate::verdict::{Commit, mark};

const ME: &str = "steward[bot]";

/// Two functions with the same line in each, then ten more lines.
fn code(one: &str, two: &str) -> String {
    let rest: String = (1..=10).map(|i| format!("line {i}\n")).collect();
    format!(
        "top\n\ndef one():\n    flag = {one}\n    return flag\n\ndef two():\n    flag = {two}\n    return flag\n\n{rest}"
    )
}

/// A repo: main with a.py, and a PR's branch `feat` off it that sets
/// one()'s flag. Main has moved on since: a line atop a.py (moving where
/// the PR's edit sits), its first line (in the edit's context) and b.txt.
struct Rig {
    d: tempfile::TempDir,
    work: PathBuf,
    reviewed: String,
}

impl Rig {
    fn new() -> Rig {
        let d = tempfile::tempdir().unwrap();
        let work = d.path().join("work");
        fs::create_dir_all(&work).unwrap();
        let w = &work;
        sh(w, &["init", "-q", "-b", "main"]);
        put(w, "a.py", &code("false", "false"));
        sh(w, &["add", "-A"]);
        sh(w, &["commit", "-qm", "base"]);
        sh(w, &["checkout", "-qb", "feat"]);
        put(w, "a.py", &code("true", "false"));
        sh(w, &["add", "-A"]);
        sh(w, &["commit", "-qm", "the PR"]);
        let reviewed = sh(w, &["rev-parse", "HEAD"]).trim().to_string();
        sh(w, &["checkout", "-q", "main"]);
        let moved = format!(
            "import x\n{}",
            code("false", "false").replacen("top", "top!", 1)
        );
        put(w, "a.py", &moved);
        put(w, "b.txt", "main's file\n");
        sh(w, &["add", "-A"]);
        sh(w, &["commit", "-qm", "main moves on"]);
        sh(w, &["checkout", "-q", "feat"]);
        Rig { d, work, reviewed }
    }

    fn w(&self) -> &Path {
        &self.work
    }

    fn head(&self) -> String {
        sh(self.w(), &["rev-parse", "HEAD"]).trim().to_string()
    }

    fn commit(&self, path: &str, text: &str) {
        put(self.w(), path, text);
        sh(self.w(), &["add", "-A"]);
        sh(self.w(), &["commit", "-qm", "more"]);
    }

    /// Those of `heads` whose looks cover feat's head, on main.
    fn carried(&self, heads: &[&str]) -> Vec<String> {
        let head = self.head();
        let at = At {
            m: self.w(),
            base: "refs/heads/main",
            head: &head,
        };
        let heads: Vec<String> = heads.iter().map(|h| h.to_string()).collect();
        at.carried(&heads).unwrap()
    }

    /// Whether the reviewed head's look covers feat's head.
    fn carries(&self) -> bool {
        self.carried(&[&self.reviewed]) == [self.reviewed.clone()]
    }

    /// git merge, which may conflict.
    fn merge(&self) -> bool {
        Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(["-c", "commit.gpgsign=false", "-C"])
            .arg(self.w())
            .args(["merge", "-q", "--no-edit", "main"])
            .output()
            .unwrap()
            .status
            .success()
    }
}

#[test]
fn a_merge_of_the_base_rebase_or_empty_commit_carries_the_look() {
    let r = Rig::new();
    assert!(r.merge());
    assert_ne!(r.head(), r.reviewed);
    assert!(r.carries());
    let r = Rig::new();
    sh(r.w(), &["rebase", "-q", "main"]);
    assert!(r.carries());
    let r = Rig::new();
    sh(r.w(), &["commit", "-q", "--allow-empty", "-m", "nothing"]);
    assert!(r.carries());
    // The head itself is no earlier head, but its own look covers it.
    let r = Rig::new();
    assert!(r.carries());
}

#[test]
fn anything_of_the_prs_own_that_moves_doesnt() {
    // The same edit moved to the other function: the same lines added and
    // removed, somewhere else.
    let r = Rig::new();
    r.commit("a.py", &code("false", "true"));
    assert!(!r.carries());
    // A commit since, merged with main or not.
    let r = Rig::new();
    r.commit("c.txt", "more\n");
    assert!(!r.carries());
    let r = Rig::new();
    assert!(r.merge());
    r.commit("c.txt", "more\n");
    assert!(!r.carries());
    // A merge that slips in an edit of its own.
    let r = Rig::new();
    assert!(r.merge());
    let text = fs::read_to_string(r.w().join("a.py")).unwrap();
    put(r.w(), "a.py", &text.replace("line 9", "line nine"));
    sh(r.w(), &["add", "-A"]);
    sh(r.w(), &["commit", "-q", "--amend", "--no-edit"]);
    assert!(!r.carries());
    // A mode change alone.
    let r = Rig::new();
    sh(r.w(), &["update-index", "--chmod=+x", "a.py"]);
    sh(r.w(), &["commit", "-qm", "executable"]);
    assert!(!r.carries());
    // main takes the PR's line, and the merge's resolution edits it.
    let r = Rig::new();
    sh(r.w(), &["checkout", "-q", "main"]);
    r.commit("a.py", &code("maybe", "false"));
    sh(r.w(), &["checkout", "-q", "feat"]);
    assert!(!r.merge(), "it conflicts");
    r.commit("a.py", &code("true or maybe", "false"));
    assert!(!r.carries());
}

#[test]
fn a_head_that_cant_be_checked_carries_nothing() {
    let r = Rig::new();
    assert!(r.merge());
    // Not in the repo: its look might be newer findings on the same change.
    let gone = "c".repeat(40);
    assert_eq!(r.carried(&[&r.reviewed, &gone]), Vec::<String>::new());
    assert_eq!(
        r.carried(&[&r.reviewed, "--output=x"]),
        Vec::<String>::new()
    );
    // One that's there and isn't the change is just not carried.
    sh(r.w(), &["checkout", "-q", "-b", "other", "main"]);
    r.commit("d.txt", "other\n");
    let other = r.head();
    sh(r.w(), &["checkout", "-q", "feat"]);
    assert_eq!(r.carried(&[&r.reviewed, &other]), vec![r.reviewed.clone()]);
}

/// The steward's review for `reviewer` of `of` with `k` findings, posted at
/// `at`, on `base`.
fn review(reviewer: &str, of: &str, k: usize, at: &str, base: &str) -> Review {
    Review {
        author: json!({"login": "steward", "__typename": "Bot"}),
        body: Some(mark(reviewer, of, base, k, Some("4f2a"))),
        commit: Some(Commit { oid: of.into() }),
        submitted_at: Some(at.into()),
    }
}

/// GitHub, as a bare repo at the config's `git`, with main and PR 5's head
/// pushed: the rig's config.
fn github(r: &Rig) -> Config {
    let mut cfg = config(r.d.path());
    cfg.snapshot.skip_paths = vec![r"\.pem$".into()];
    let origin = Path::new(&cfg.git).join("sam/r.git");
    fs::create_dir_all(&origin).unwrap();
    sh(&origin, &["init", "-q", "--bare"]);
    let to = origin.to_str().unwrap();
    sh(r.w(), &["push", "-qf", to, "main", "HEAD:refs/pull/5/head"]);
    cfg
}

fn ask(r: &Rig, reviews: Vec<Review>, kept: Option<Carry>) -> Ask {
    Ask {
        number: 5,
        head: r.head(),
        base: "main".into(),
        reviewers: vec!["codex".into()],
        reviews,
        kept,
    }
}

#[test]
fn it_is_worked_out_from_the_mirror_once_for_each_set_of_looks() {
    let r = Rig::new();
    let cfg = github(&r);
    // Nothing to carry: no look, or none clean.
    assert_eq!(
        carry(&cfg, "sam/r", ME, &ask(&r, vec![], None), false),
        Ok(None)
    );
    let found = vec![review(
        "codex",
        &r.reviewed,
        1,
        "2026-10-24T00:00:00Z",
        "main",
    )];
    assert_eq!(
        carry(&cfg, "sam/r", ME, &ask(&r, found, None), false),
        Ok(None)
    );
    // A clean look at the head before main was merged in carries, and so
    // would one on another base, if the PR goes back to it.
    assert!(r.merge());
    put(r.w(), "key.pem", "SECRET\n");
    sh(r.w(), &["add", "-A"]);
    sh(r.w(), &["commit", "-q", "--amend", "--no-edit"]);
    let cfg = github(&r);
    let clean = vec![
        review("codex", &r.reviewed, 0, "2026-10-24T00:00:00Z", "main"),
        review(
            "codex",
            &"d".repeat(40),
            0,
            "2026-10-24T00:00:00Z",
            "release",
        ),
    ];
    let got = carry(&cfg, "sam/r", ME, &ask(&r, clean.clone(), None), false)
        .unwrap()
        .unwrap();
    // The PR's own key, slipped into the merge, is an edit of its own.
    assert_eq!(got.carried, Vec::<String>::new());
    let r = Rig::new();
    put(r.w(), "key.pem", "SECRET\n");
    sh(r.w(), &["add", "-A"]);
    sh(r.w(), &["commit", "-q", "--amend", "--no-edit"]);
    let reviewed = r.head();
    assert!(r.merge());
    let cfg = github(&r);
    let clean = vec![review(
        "codex",
        &reviewed,
        0,
        "2026-10-24T00:00:00Z",
        "main",
    )];
    let got = carry(&cfg, "sam/r", ME, &ask(&r, clean.clone(), None), false)
        .unwrap()
        .unwrap();
    assert_eq!(
        got,
        Carry {
            head: r.head(),
            base: "main".into(),
            heads: vec![reviewed.clone()],
            carried: vec![reviewed.clone()],
            // A judge still looks at what the reviewers weren't shown.
            unseen: vec!["key.pem".into()],
        }
    );
    // Kept, it's the answer while the head and the looks are the same,
    // and a status run reads it without fetching anything.
    let mut kept = got.clone();
    kept.unseen = vec!["kept".into()];
    let again = carry(
        &cfg,
        "sam/r",
        ME,
        &ask(&r, clean.clone(), Some(kept.clone())),
        true,
    );
    assert_eq!(again, Ok(Some(kept.clone())));
    let dry = carry(&cfg, "sam/r", ME, &ask(&r, clean.clone(), None), true);
    assert_eq!(dry, Ok(None));
    // A look at another head since is checked: this one (before the key
    // was amended in) isn't in the mirror, and might have found something
    // in the same change, so nothing carries.
    let mut more = clean.clone();
    more.push(review(
        "codex",
        &r.reviewed,
        1,
        "2026-10-24T01:00:00Z",
        "main",
    ));
    let got = carry(&cfg, "sam/r", ME, &ask(&r, more, Some(kept)), false)
        .unwrap()
        .unwrap();
    assert_eq!((got.heads.len(), got.carried), (2, vec![]));
}

#[test]
fn only_the_stewards_own_looks_count_and_the_ask_is_read_strictly() {
    let r = Rig::new();
    let mut forged = review("codex", &r.reviewed, 0, "2026-10-24T00:00:00Z", "main");
    forged.author = json!({"login": "mallory", "__typename": "User"});
    let cfg = github(&r);
    assert_eq!(
        carry(&cfg, "sam/r", ME, &ask(&r, vec![forged], None), false),
        Ok(None)
    );
    let mut a = ask(&r, vec![], None);
    a.head = "--output=x".into();
    assert_eq!(a.wrong(), Some("\"--output=x\" isn't a commit's id".into()));
    let mut a = ask(&r, vec![], None);
    a.reviewers = vec!["Codex".into()];
    assert_eq!(a.wrong(), Some("\"Codex\" isn't a reviewer's name".into()));
}
