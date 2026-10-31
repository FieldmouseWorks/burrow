// When a job is over, so its scratch goes (said `proj done`, or stopped, or an
// --owned job's turn that awaits nothing; never one that awaits or asks, and
// only as its latest turn left it), and that clearing it takes its worktrees,
// however deep, off their repos too, so their branches check out again, leaves
// every other job's alone, gets past what a build left read-only, and lets two
// clears at once both succeed.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::slice::from_ref;
use std::thread;

use super::{clear, dir, over};
use crate::config::Config;
use crate::config::tests::example;
use crate::job::Job;

struct Box {
    root: tempfile::TempDir,
    cfg: Config,
}

impl Box {
    fn new() -> Box {
        let root = tempfile::tempdir().unwrap();
        let mut cfg: Config = serde_json::from_value(example()).unwrap();
        cfg.tools.git = "git".into();
        cfg.jobs = root.path().join("jobs");
        cfg.scratch = root.path().join("scratch");
        fs::create_dir_all(cfg.scratch.join("jobs")).unwrap();
        Box { root, cfg }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.path().join(rel)
    }

    /// A repo with a commit on main.
    fn repo(&self, name: &str) -> PathBuf {
        let repo = self.path(name);
        fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "one"]);
        repo
    }

    /// A worktree of the repo at a path, on a new branch.
    fn worktree(&self, repo: &Path, branch: &str, at: &Path) {
        let at = at.to_str().unwrap();
        git(repo, &["worktree", "add", "-q", "-b", branch, at, "main"]);
    }

    /// Whether a job as these files leave it is over: "turns N", "N.end KIND",
    /// or the name of a file to make.
    fn over(&self, files: &[&str]) -> bool {
        let job = Job::new(&self.cfg, "j");
        let _ = fs::remove_dir_all(&job.dir);
        fs::create_dir_all(&job.dir).unwrap();
        for f in files {
            match f.split_once(' ') {
                Some(("turns", n)) => fs::write(job.path("turns"), format!("{n}\n")).unwrap(),
                Some((end, kind)) => fs::write(job.path(end), format!("{kind}\n")).unwrap(),
                None if *f == "await" => fs::write(job.path(f), "o/r#1\t1000\t{}\n").unwrap(),
                None => fs::write(job.path(f), "").unwrap(),
            }
        }
        over(&job)
    }
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

/// The worktrees git lists for a repo.
fn trees(repo: &Path) -> Vec<PathBuf> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .unwrap();
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter_map(|l| l.strip_prefix("worktree ").map(PathBuf::from))
        .collect()
}

/// What's been left aside in the scratch's jobs dir.
fn left(cfg: &Config) -> Vec<PathBuf> {
    fs::read_dir(cfg.scratch.join("jobs"))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(".gone-"))
        .map(|e| e.path())
        .collect()
}

#[test]
fn when_a_job_is_over() {
    let b = Box::new();
    assert!(!b.over(&[]));
    // Said it's done: over. Waiting on GitHub or on a person, or ended any
    // other way: a reply or what it awaits resumes it, in its scratch.
    assert!(b.over(&["turns 1", "1.end done"]));
    assert!(!b.over(&["turns 1", "1.end await", "await"]));
    assert!(!b.over(&["turns 1", "1.end ask"]));
    assert!(!b.over(&["turns 1", "1.end undeclared"]));
    assert!(!b.over(&["turns 1"]));
    // The latest turn's word is what counts.
    assert!(!b.over(&["turns 2", "1.end done", "2.end ask"]));
    assert!(b.over(&["turns 2", "1.end ask", "2.end done"]));
    // --owned: its starter starts a new job for anything more, so a turn that
    // awaits nothing ends it, however it ended; one that awaits doesn't.
    assert!(b.over(&["turns 1", "owned"]));
    assert!(b.over(&["turns 3", "owned", "3.end ask"]));
    assert!(!b.over(&["turns 1", "owned", "1.end await", "await"]));
    // Stopped: over, whatever it was waiting on.
    assert!(b.over(&["turns 1", "1.end await", "await", "1.stopped"]));
    assert!(b.over(&["turns 2", "2.end ask", "2.stopped"]));
    // A stop marks the turn it stopped: an earlier one's mark is history.
    assert!(!b.over(&["turns 2", "1.stopped", "2.end ask"]));
}

#[test]
fn clearing_takes_its_worktrees_off_their_repos() {
    let b = Box::new();
    let cfg = &b.cfg;
    let (a, other) = (dir(cfg, "a"), dir(cfg, "b"));
    // A repo, and two jobs with worktrees of it, one with a clone as well.
    let web = b.repo("web");
    b.worktree(&web, "fix-a", &a.join("web"));
    b.worktree(&web, "fix-b", &other.join("web"));
    git(
        b.root.path(),
        &["clone", "-q", "web", a.join("clone").to_str().unwrap()],
    );
    // And, deep in a's scratch, the only worktree of a second repo.
    let api = b.repo("api");
    b.worktree(&api, "fix-deep", &a.join("x/y/z/w/api"));
    // What a build leaves, some of it read-only.
    let ro = a.join("web/target/debug/ro");
    fs::create_dir_all(&ro).unwrap();
    fs::write(ro.join("lib.rlib"), vec![0; 1024]).unwrap();
    fs::set_permissions(&ro, fs::Permissions::from_mode(0o555)).unwrap();

    clear(cfg, "a");
    assert!(!a.exists(), "job a's scratch is still there");
    assert_eq!(left(cfg), Vec::<PathBuf>::new());
    // Its worktree is off the repo, so its branch checks out again; b's is kept.
    assert_eq!(trees(&web), [web.clone(), other.join("web")]);
    git(
        &web,
        &[
            "worktree",
            "add",
            "-q",
            b.path("again").to_str().unwrap(),
            "fix-a",
        ],
    );
    assert!(
        other.join("web/.git").exists(),
        "b's worktree went with a's"
    );
    assert_eq!(trees(&api), from_ref(&api));

    // A job with no scratch (a sandbox's, or one cleared already): nothing to do.
    clear(cfg, "a");
    clear(cfg, "never");
}

// A name that isn't one job's can't reach past the scratch's jobs dir.
#[test]
fn clearing_only_ever_takes_one_jobs() {
    let b = Box::new();
    let cfg = &b.cfg;
    let beside = cfg.scratch.join("beside");
    fs::create_dir_all(&beside).unwrap();
    fs::create_dir_all(dir(cfg, "a/b")).unwrap();
    for id in ["", ".", "..", "../beside", "a/b"] {
        clear(cfg, id);
    }
    assert!(beside.exists());
    assert!(cfg.scratch.join("jobs").exists());
    assert!(dir(cfg, "a/b").exists());
}

// The turn's end and proj stop clearing at once: both are fine, and it goes once.
#[test]
fn two_clears_at_once() {
    let b = Box::new();
    let cfg = &b.cfg;
    let web = b.repo("web");
    let c = dir(cfg, "c");
    for i in 0..5 {
        b.worktree(&web, &format!("race-{i}"), &c.join("web"));
        fs::create_dir_all(c.join("web/target")).unwrap();
        for f in 0..500 {
            fs::write(c.join(format!("web/target/f{f}")), "").unwrap();
        }
        thread::scope(|s| {
            s.spawn(|| clear(cfg, "c"));
            clear(cfg, "c");
        });
        assert!(!c.exists(), "c's scratch survived two clears");
        assert_eq!(
            trees(&web),
            from_ref(&web),
            "a raced worktree is still listed"
        );
    }
    assert_eq!(left(cfg), Vec::<PathBuf>::new());
}
