// When a job is over, so its scratch goes (said `proj done`, or stopped, or an
// --owned job's turn that awaits nothing; never one that awaits or asks, and
// only as its latest turn left it), and that clearing it takes its worktrees,
// however deep, off their repos too, so their branches check out again, leaves
// every other job's alone, gets past what a build left read-only, and lets two
// clears at once both succeed.

use std::fs;
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
        cfg.projects = root.path().to_path_buf();
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
        let _ = fs::remove_dir_all(crate::ask::host_path(&job, ""));
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

/// Git in `dir` as the tests run it: the machine's own config (an identity
/// or not, a global ignore) never reaches it, so a test does the same here
/// as in the build sandbox, which has none.
fn git_in(dir: &Path) -> Command {
    let mut c = Command::new("git");
    c.arg("-C")
        .arg(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t");
    c
}

fn git(dir: &Path, args: &[&str]) {
    let status = git_in(dir).args(args).status().unwrap();
    assert!(status.success(), "git {args:?}");
}

/// What a git command in `dir` prints, trimmed.
fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = git_in(dir).args(args).output().unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// The worktrees git lists for a repo.
fn trees(repo: &Path) -> Vec<PathBuf> {
    git_out(repo, &["worktree", "list", "--porcelain"])
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
    // Both repos are the job's project's, as far as the test is concerned.
    grant(&b, "a");
    // What a build leaves, some of it read-only.
    let ro = a.join("web/target/debug/ro");
    fs::create_dir_all(&ro).unwrap();
    fs::write(ro.join("lib.rlib"), vec![0; 1024]).unwrap();
    burrow_test::permissions(&ro, 0o555);

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

/// The job's project is the folder the test's repos are in.
fn grant(b: &Box, id: &str) {
    grant_to(b, id, b.root.path());
}

/// The project the host granted job `id` at launch, in its request.
fn grant_to(b: &Box, id: &str, project: &Path) {
    let job = Job::new(&b.cfg, id);
    fs::create_dir_all(&job.dir).unwrap();
    crate::job::Request {
        project: project.display().to_string(),
        ..Default::default()
    }
    .save(&job)
    .unwrap();
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
        grant(&b, "c");
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

/// A job's worktree with a tracked file edited and an untracked one made, as a
/// turn refused for usage leaves it, and the job stopped (the old, destructive
/// way to give up a waiting turn).
fn unfinished(b: &Box) -> PathBuf {
    let repo = b.repo("repo");
    fs::write(repo.join("tracked.txt"), "one\n").unwrap();
    git(&repo, &["add", "tracked.txt"]);
    git(&repo, &["commit", "-q", "-m", "two"]);
    let wt = dir(&b.cfg, "u").join("wt");
    fs::create_dir_all(wt.parent().unwrap()).unwrap();
    b.worktree(&repo, "work", &wt);
    fs::write(wt.join("tracked.txt"), "edited\n").unwrap();
    fs::create_dir_all(wt.join("sub")).unwrap();
    fs::write(wt.join("sub/new.txt"), "fresh\n").unwrap();
    fs::write(wt.join(".gitignore"), "target/\n").unwrap();
    fs::create_dir_all(wt.join("target")).unwrap();
    fs::write(wt.join("target/out"), "build\n").unwrap();
    let job = Job::new(&b.cfg, "u");
    fs::create_dir_all(&job.dir).unwrap();
    fs::write(job.path("turns"), "1\n").unwrap();
    fs::write(job.turn("1", "stopped"), "").unwrap();
    grant_to(b, "u", &repo);
    wt
}

// Clearing a stopped job (what withdrawing a waiting turn does) once removed
// its unfinished files with the rest. They are kept, outside the scratch, as a
// patch and a copy; a build's output is not.
#[test]
fn clearing_keeps_unfinished_work() {
    let b = Box::new();
    unfinished(&b);
    clear(&b.cfg, "u");
    assert!(!dir(&b.cfg, "u").exists(), "the scratch still goes");
    let kept = Job::new(&b.cfg, "u").path("preserved/wt.1");
    let patch = fs::read_to_string(kept.join("tracked.patch")).unwrap();
    assert!(patch.contains("+edited"), "{patch}");
    assert_eq!(
        fs::read_to_string(kept.join("untracked/sub/new.txt")).unwrap(),
        "fresh\n"
    );
    assert_eq!(
        fs::read_to_string(kept.join("branch")).unwrap().trim(),
        "work"
    );
    assert!(
        !kept.join("untracked/target").exists(),
        "build output stays disposable"
    );
}

// Work that can't be kept keeps the scratch where it is.
#[test]
fn clearing_keeps_the_scratch_when_work_cant_be_saved() {
    let b = Box::new();
    let wt = unfinished(&b);
    fs::write(Job::new(&b.cfg, "u").path("preserved"), "in the way").unwrap();
    clear(&b.cfg, "u");
    assert_eq!(
        fs::read_to_string(wt.join("sub/new.txt")).unwrap(),
        "fresh\n"
    );
}

// A handoff in progress is the new turn's scratch: nothing clears it.
#[test]
fn clearing_waits_for_a_handoff() {
    let b = Box::new();
    let wt = unfinished(&b);
    let job = Job::new(&b.cfg, "u");
    fs::write(job.path(super::HANDOFF), "1\n").unwrap();
    assert!(!over(&job));
    clear(&b.cfg, "u");
    assert_eq!(
        fs::read_to_string(wt.join("tracked.txt")).unwrap(),
        "edited\n"
    );
    assert!(!job.path("preserved").exists());
}

fn kept(b: &Box, name: &str) -> PathBuf {
    Job::new(&b.cfg, "u").path(&format!("preserved/{name}"))
}

/// A clone of a repo inside the job's scratch, its own repository there.
fn clone_in_scratch(b: &Box) -> PathBuf {
    let repo = b.repo("origin");
    let at = dir(&b.cfg, "u").join("clone");
    fs::create_dir_all(at.parent().unwrap()).unwrap();
    git(
        b.root.path(),
        &["clone", "-q", repo.to_str().unwrap(), at.to_str().unwrap()],
    );
    fs::create_dir_all(Job::new(&b.cfg, "u").dir).unwrap();
    at
}

// The job wrote its repository's config, and keeping its work runs on the
// host: a `core.fsmonitor` it set must not run there.
#[test]
fn keeping_work_runs_nothing_the_job_configured() {
    let b = Box::new();
    let at = clone_in_scratch(&b);
    let ran = |what: &str| b.path(what);
    for (key, what) in [
        ("core.fsmonitor", "fsmonitor"),
        ("diff.external", "external"),
        ("filter.x.clean", "filter"),
    ] {
        git(
            &at,
            &[
                "config",
                key,
                &format!("touch {}; cat", ran(what).display()),
            ],
        );
    }
    fs::write(at.join(".gitattributes"), "* filter=x\n").unwrap();
    fs::write(at.join("f.txt"), "work\n").unwrap();
    fs::write(at.join("g.txt"), "more\n").unwrap();
    git(&at, &["add", "-N", "g.txt"]);
    // What the setup itself ran is not the host's doing.
    for what in ["fsmonitor", "external", "filter"] {
        let _ = fs::remove_file(ran(what));
    }
    clear(&b.cfg, "u");
    for what in ["fsmonitor", "external", "filter"] {
        assert!(!ran(what).exists(), "{what} command ran on the host");
    }
    assert_eq!(
        fs::read_to_string(kept(&b, "clone.1").join("untracked/f.txt")).unwrap(),
        "work\n"
    );
}

// Staged content is kept: before a first commit, and when the working file
// goes back to head after staging an edit.
#[test]
fn keeping_work_keeps_the_index() {
    let b = Box::new();
    let at = dir(&b.cfg, "u").join("born");
    fs::create_dir_all(&at).unwrap();
    git(&at, &["init", "-q", "-b", "main"]);
    fs::write(at.join("first.txt"), "staged only\n").unwrap();
    git(&at, &["add", "first.txt"]);
    let at2 = clone_in_scratch(&b);
    fs::write(at2.join("README"), "v1\n").unwrap();
    git(&at2, &["add", "README"]);
    git(&at2, &["commit", "-q", "-m", "r"]);
    fs::write(at2.join("README"), "staged edit\n").unwrap();
    git(&at2, &["add", "README"]);
    fs::write(at2.join("README"), "v1\n").unwrap();
    clear(&b.cfg, "u");
    let unborn = fs::read_to_string(kept(&b, "born.1").join("staged.patch")).unwrap();
    assert!(unborn.contains("+staged only"), "{unborn}");
    let k = kept(&b, "clone.1");
    let staged = fs::read_to_string(k.join("staged.patch")).unwrap();
    assert!(staged.contains("+staged edit"), "{staged}");
    assert_eq!(fs::read_to_string(k.join("tracked.patch")).unwrap(), "");
}

// A clone's objects go with the scratch, so what its patches apply to is kept
// as a bundle: an unpushed commit, then further edits.
#[test]
fn keeping_work_keeps_the_base_of_a_clone() {
    let b = Box::new();
    let at = clone_in_scratch(&b);
    fs::write(at.join("a.txt"), "committed\n").unwrap();
    git(&at, &["add", "a.txt"]);
    git(&at, &["commit", "-q", "-m", "local"]);
    fs::write(at.join("a.txt"), "edited later\n").unwrap();
    clear(&b.cfg, "u");
    let k = kept(&b, "clone.1");
    let head = fs::read_to_string(k.join("head")).unwrap();
    let bundle = k.join("base.bundle");
    let heads = git_out(
        &b.path("origin"),
        &["bundle", "list-heads", bundle.to_str().unwrap()],
    );
    assert!(heads.contains(head.trim()));
}

// What can't be looked at is not nothing: a directory that can't be read stops
// the clearing.
#[test]
fn an_unreadable_directory_keeps_the_scratch() {
    let b = Box::new();
    let at = dir(&b.cfg, "u").join("hidden");
    fs::create_dir_all(&at).unwrap();
    fs::write(at.join("f"), "x").unwrap();
    burrow_test::permissions(&at, 0o0);
    if fs::read_dir(&at).is_ok() {
        return; // as root, nothing is unreadable
    }
    clear(&b.cfg, "u");
    burrow_test::permissions(&at, 0o700);
    assert!(at.join("f").exists());
}

// Clears at once each find a whole copy, one, never another's half of one.
#[test]
fn clears_at_once_keep_one_whole_copy() {
    let b = Box::new();
    let wt = unfinished(&b);
    fs::write(wt.join("big"), vec![7u8; 4 << 20]).unwrap();
    let cfg = &b.cfg;
    thread::scope(|s| {
        for _ in 0..6 {
            s.spawn(|| clear(cfg, "u"));
        }
    });
    let base = Job::new(cfg, "u").path("preserved");
    let names: Vec<_> = fs::read_dir(&base)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["wt.1".to_owned()], "{names:?}");
    assert_eq!(
        fs::metadata(base.join("wt.1/untracked/big")).unwrap().len(),
        4 << 20
    );
    assert!(base.join("wt.1/tracked.patch").exists());
}

fn keep_err(b: &Box) -> String {
    super::preserve::unfinished(&b.cfg, "u").unwrap_err()
}

// What a job links its repository to is read only inside what it was given:
// its scratch and its project. A gitfile, a commondir, a HEAD or a ref that
// points elsewhere keeps the scratch, reads nothing there, and prints none of
// it.
#[test]
fn keeping_work_reads_only_what_the_job_was_given() {
    let b = Box::new();
    let wt = unfinished(&b);
    let other = b.repo("other");
    fs::write(other.join("secret.txt"), "TOPSECRET\n").unwrap();
    git(&other, &["add", "secret.txt"]);
    git(&other, &["commit", "-q", "-m", "s"]);
    let dotgit = wt.join(".git");
    let original = fs::read_to_string(&dotgit).unwrap();
    // A gitfile at another repository.
    fs::write(
        &dotgit,
        format!("gitdir: {}\n", other.join(".git").display()),
    )
    .unwrap();
    let e = keep_err(&b);
    assert!(e.contains("outside the job's granted paths"), "{e}");
    assert!(!job_kept(&b));
    fs::write(&dotgit, &original).unwrap();
    let gitdir = PathBuf::from(original.trim().strip_prefix("gitdir: ").unwrap());
    // A commondir at another repository.
    fs::write(
        gitdir.join("commondir"),
        other.join(".git").to_str().unwrap(),
    )
    .unwrap();
    assert!(keep_err(&b).contains("outside"));
    fs::write(gitdir.join("commondir"), "../..").unwrap();
    // A HEAD that is a link to a secret: its content is not repeated.
    fs::remove_file(gitdir.join("HEAD")).unwrap();
    std::os::unix::fs::symlink(other.join("secret.txt"), gitdir.join("HEAD")).unwrap();
    let e = keep_err(&b);
    assert!(!e.contains("TOPSECRET"), "{e}");
    assert!(!job_kept(&b));
    fs::remove_file(gitdir.join("HEAD")).unwrap();
    // A HEAD naming a ref outside refs/, and a ref that is a link to a secret.
    fs::write(gitdir.join("HEAD"), "ref: ../../../other/secret.txt\n").unwrap();
    assert!(keep_err(&b).contains("names no ref"));
    fs::write(gitdir.join("HEAD"), "ref: refs/heads/work\n").unwrap();
    let r = b.path("repo/.git/refs/heads/work");
    let _ = fs::remove_file(&r);
    std::os::unix::fs::symlink(other.join("secret.txt"), &r).unwrap();
    let e = keep_err(&b);
    assert!(!e.contains("TOPSECRET"), "{e}");
    assert!(!job_kept(&b));
}

fn job_kept(b: &Box) -> bool {
    Job::new(&b.cfg, "u").path("preserved/wt.1").exists()
}

// Object stores count as links too: an alternate of an alternate that leaves
// the grants is refused, and one that leads back into the scratch gets the
// base bundled.
#[test]
fn keeping_work_follows_alternates_all_the_way() {
    let b = Box::new();
    let at = clone_in_scratch(&b);
    fs::write(at.join("a.txt"), "committed\n").unwrap();
    git(&at, &["add", "a.txt"]);
    git(&at, &["commit", "-q", "-m", "local"]);
    fs::write(at.join("a.txt"), "edited\n").unwrap();
    // The clone's objects are an outside store's alternate, which is
    // another store's, which is the clone's own again.
    let mid = b.path("mid/objects");
    fs::create_dir_all(mid.join("info")).unwrap();
    let own = at.join(".git/objects");
    fs::write(mid.join("info/alternates"), format!("{}\n", own.display())).unwrap();
    fs::write(own.join("info/alternates"), format!("{}\n", mid.display())).unwrap();
    assert!(keep_err(&b).contains("outside the job's granted paths"));
    // Granted, through the project: the chain ends in the scratch, so the
    // base is bundled.
    grant_to(&b, "u", &b.path("mid"));
    super::preserve::unfinished(&b.cfg, "u").unwrap();
    assert!(kept(&b, "clone.1").join("base.bundle").exists());
}

// A commit made in a submodule that the superproject now points at exists
// nowhere else: its tree is kept though its files are clean.
#[test]
fn keeping_work_keeps_a_clean_submodule_whose_commit_moved() {
    let b = Box::new();
    let sub = b.repo("subsrc");
    let at = clone_in_scratch(&b);
    git(
        &at,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            sub.to_str().unwrap(),
            "mod",
        ],
    );
    git(&at, &["commit", "-q", "-m", "add sub"]);
    let inner = at.join("mod");
    fs::write(inner.join("x.txt"), "inside\n").unwrap();
    git(&inner, &["add", "x.txt"]);
    git(&inner, &["commit", "-q", "-m", "unpublished"]);
    git(&at, &["add", "mod"]);
    clear(&b.cfg, "u");
    let k = kept(&b, "clone-mod.1");
    assert!(k.join("base.bundle").exists(), "{:?}", left(&b.cfg));
    let outer = fs::read_to_string(kept(&b, "clone.1").join("staged.patch")).unwrap();
    assert!(outer.contains("Subproject commit"), "{outer}");
}

// The commit the index names in a submodule is kept though the submodule is
// checked out at another: its bundle carries both.
#[test]
fn keeping_work_keeps_a_staged_submodule_commit_that_is_not_checked_out() {
    let b = Box::new();
    let sub = b.repo("subsrc");
    let at = clone_in_scratch(&b);
    git(
        &at,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            sub.to_str().unwrap(),
            "mod",
        ],
    );
    git(&at, &["commit", "-q", "-m", "add sub"]);
    let inner = at.join("mod");
    fs::write(inner.join("x.txt"), "inside\n").unwrap();
    git(&inner, &["add", "x.txt"]);
    git(&inner, &["commit", "-q", "-m", "unpublished"]);
    let staged = git_out(&inner, &["rev-parse", "HEAD"]);
    git(&at, &["add", "mod"]);
    git(&inner, &["checkout", "-q", "--detach", "HEAD~1"]);
    clear(&b.cfg, "u");
    let bundle = kept(&b, "clone-mod.1").join("base.bundle");
    assert!(bundle.exists(), "{:?}", left(&b.cfg));
    let heads = git_out(
        bundle.parent().unwrap(),
        &["bundle", "list-heads", "base.bundle"],
    );
    assert!(heads.contains(&staged), "{heads}");
}

// Ignore rules kept in the repository's own info/exclude still apply, so a
// build's output they name isn't archived.
#[test]
fn keeping_work_honors_the_repositorys_exclude_file() {
    let b = Box::new();
    let at = clone_in_scratch(&b);
    fs::write(at.join(".git/info/exclude"), "out/\n").unwrap();
    fs::create_dir_all(at.join("out")).unwrap();
    fs::write(at.join("out/big"), "build\n").unwrap();
    fs::write(at.join("real.txt"), "work\n").unwrap();
    clear(&b.cfg, "u");
    let k = kept(&b, "clone.1");
    assert!(k.join("untracked/real.txt").exists());
    assert!(!k.join("untracked/out").exists());
}

// Where a user's global ignore file is when nothing configures it.
#[test]
fn the_global_ignore_file_is_where_git_looks() {
    use std::ffi::OsString;
    let os = |s: &str| Some(OsString::from(s));
    assert_eq!(
        super::preserve::default_global_ignore(os("/x"), os("/h")),
        Some(PathBuf::from("/x/git/ignore"))
    );
    assert_eq!(
        super::preserve::default_global_ignore(os(""), os("/h")),
        Some(PathBuf::from("/h/.config/git/ignore"))
    );
    assert_eq!(super::preserve::default_global_ignore(None, None), None);
}

// A preserved directory that is a link to elsewhere isn't written through.
#[test]
fn keeping_work_never_writes_through_a_link() {
    let b = Box::new();
    let wt = unfinished(&b);
    let elsewhere = b.path("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, Job::new(&b.cfg, "u").path("preserved")).unwrap();
    clear(&b.cfg, "u");
    assert!(wt.join("tracked.txt").exists(), "the scratch stays");
    assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
}

// The sanitized config keeps the real object format and the whole ignore path.
#[test]
fn sanitized_config_reads_the_setting_and_quotes_paths() {
    use super::preserve::{object_format_is_sha256 as sha, quote_config};
    assert!(!sha("[remote \"o\"]\n\turl = https://x/sha256-tool\n"));
    assert!(sha(
        "[core]\n\trepositoryformatversion = 1\n[extensions]\n\tObjectFormat = SHA256\n"
    ));
    assert!(!sha("[extensions]\n\tobjectformat = sha1\n"));
    assert_eq!(quote_config("/a #b/c;d"), "\"/a #b/c;d\"");
    assert_eq!(quote_config("/a\\\"b"), "\"/a\\\\\\\"b\"");
}

// A project the job names in its own state grants nothing: only the host's
// record of what it launched the job in does, and only a project of the
// machine's.
#[test]
fn a_project_the_job_names_itself_grants_nothing() {
    let b = Box::new();
    let wt = unfinished(&b);
    let other = b.repo("other");
    let dotgit = wt.join(".git");
    fs::write(
        &dotgit,
        format!("gitdir: {}\n", other.join(".git").display()),
    )
    .unwrap();
    let job = Job::new(&b.cfg, "u");
    // The job widens its own record to everything.
    fs::write(job.path("project"), "/\n").unwrap();
    assert!(keep_err(&b).contains("outside the job's granted paths"));
    // With no request on the host at all, what the job wrote isn't adopted
    // as one, not even a project of the machine's.
    fs::remove_file(crate::ask::host_path(&job, "request")).unwrap();
    fs::write(job.path("project"), format!("{}\n", other.display())).unwrap();
    assert!(keep_err(&b).contains("outside the job's granted paths"));
    assert!(!crate::ask::host_path(&job, "request").exists());
    // A host record outside the machine's projects is no grant either.
    let mut elsewhere = Box::new();
    elsewhere.cfg.projects = elsewhere.path("only-here");
    fs::create_dir_all(&elsewhere.cfg.projects).unwrap();
    unfinished(&elsewhere);
    assert!(keep_err(&elsewhere).contains("outside the job's granted paths"));
}

fn fifo(path: &Path) {
    assert!(Command::new("mkfifo").arg(path).status().unwrap().success());
}

// Git opens object files by path, so one that is a link (a fan-out directory,
// a loose object or a pack) would send it into another repository: refused
// before git runs, and the scratch stays.
#[test]
fn keeping_work_refuses_links_inside_an_object_store() {
    let b = Box::new();
    let at = clone_in_scratch(&b);
    fs::write(at.join("a.txt"), "edited\n").unwrap();
    let other = b.repo("other");
    let objects = at.join(".git/objects");
    let fan = objects.join("ab");
    std::os::unix::fs::symlink(other.join(".git/objects"), &fan).unwrap();
    assert!(keep_err(&b).contains("neither a plain file nor a directory"));
    fs::remove_file(&fan).unwrap();
    fs::create_dir_all(objects.join("pack")).unwrap();
    let pack = objects.join("pack/x.pack");
    let _ = fs::remove_file(&pack);
    std::os::unix::fs::symlink(other.join(".git/HEAD"), &pack).unwrap();
    assert!(keep_err(&b).contains("neither a plain file nor a directory"));
    assert!(!Job::new(&b.cfg, "u").path("preserved/clone.1").exists());
    assert!(at.exists(), "the scratch stays");
}

// A metadata file that is a FIFO, or too big, is an error at once, not a wait
// or a partial read; the scratch stays.
#[test]
fn keeping_work_refuses_special_and_oversized_metadata() {
    let b = Box::new();
    let at = clone_in_scratch(&b);
    fs::write(at.join("a.txt"), "edited\n").unwrap();
    let head = at.join(".git/HEAD");
    let original = fs::read(&head).unwrap();
    fs::remove_file(&head).unwrap();
    fifo(&head);
    let e = keep_err(&b);
    assert!(e.contains("not a regular file"), "{e}");
    fs::remove_file(&head).unwrap();
    fs::write(
        &head,
        vec![b'x'; (super::preserve::META_LIMIT + 1) as usize],
    )
    .unwrap();
    assert!(keep_err(&b).contains("larger than"));
    fs::write(&head, &original).unwrap();
    let index = at.join(".git/index");
    fs::remove_file(&index).unwrap();
    fifo(&index);
    assert!(keep_err(&b).contains("not a regular file"));
    assert!(at.exists());
}

/// Two submodule levels: the outer clone's `mod`, whose own `nest` is a
/// submodule of it. A commit in `nest` that `mod`'s commit names exists only
/// there.
fn nested(b: &Box) -> (PathBuf, PathBuf, String) {
    let leaf = b.repo("leaf");
    let mid = b.repo("midsrc");
    let allow = ["-c", "protocol.file.allow=always"];
    let add = |dir: &Path, src: &Path, name: &str| {
        let mut args = allow.to_vec();
        args.extend(["submodule", "add", "-q", src.to_str().unwrap(), name]);
        git(dir, &args);
    };
    add(&mid, &leaf, "nest");
    git(&mid, &["commit", "-q", "-m", "add nest"]);
    let at = clone_in_scratch(b);
    add(&at, &mid, "mod");
    git(&at, &["commit", "-q", "-m", "add mod"]);
    let outer_mod = at.join("mod");
    let mut args = allow.to_vec();
    args.extend(["submodule", "update", "--init", "-q"]);
    git(&outer_mod, &args);
    let nest = outer_mod.join("nest");
    fs::write(nest.join("deep.txt"), "deep\n").unwrap();
    git(&nest, &["add", "deep.txt"]);
    git(&nest, &["commit", "-q", "-m", "unpublished deep"]);
    let sha = git_out(&nest, &["rev-parse", "HEAD"]);
    git(&outer_mod, &["add", "nest"]);
    git(&outer_mod, &["commit", "-q", "-m", "records deep"]);
    git(&at, &["add", "mod"]);
    // The nested commit is no longer checked out anywhere.
    git(&nest, &["checkout", "-q", "--detach", "HEAD~1"]);
    (at, nest, sha)
}

// A nested submodule's commit that only a clean, forced submodule's own
// commit names is kept too, though nothing in the outer diffs shows it.
#[test]
fn keeping_work_keeps_a_nested_submodule_commit_recursively() {
    let b = Box::new();
    let (_at, _nest, sha) = nested(&b);
    clear(&b.cfg, "u");
    let bundle = kept(&b, "clone-mod-nest.1").join("base.bundle");
    assert!(bundle.exists(), "{:?}", left(&b.cfg));
    let heads = git_out(
        bundle.parent().unwrap(),
        &["bundle", "list-heads", "base.bundle"],
    );
    assert!(heads.contains(&sha));
}

// A submodule the saved states name with no working tree keeps the scratch.
#[test]
fn keeping_work_stays_when_a_nested_submodule_has_no_worktree() {
    let b = Box::new();
    let (at, nest, _sha) = nested(&b);
    fs::remove_dir_all(&nest).unwrap();
    let e = keep_err(&b);
    assert!(e.contains("no working tree"), "{e}");
    assert!(at.exists());
}

// A tracked file marked assume-unchanged or skip-worktree and then edited is
// still work: git's own diff trusts the flag, the kept patch must not.
#[test]
fn keeping_work_sees_edits_hidden_by_index_flags() {
    for flag in ["--assume-unchanged", "--skip-worktree"] {
        let b = Box::new();
        let at = clone_in_scratch(&b);
        fs::write(at.join("README"), "v1\n").unwrap();
        git(&at, &["add", "README"]);
        git(&at, &["commit", "-q", "-m", "r"]);
        git(&at, &["update-index", flag, "README"]);
        fs::write(at.join("README"), "hidden edit\n").unwrap();
        clear(&b.cfg, "u");
        let tracked = fs::read_to_string(kept(&b, "clone.1").join("tracked.patch")).unwrap();
        assert!(tracked.contains("+hidden edit"), "{flag}: {tracked}");
    }
}

// A sparse checkout's absent, skip-worktree file is not a deletion.
#[test]
fn keeping_work_does_not_call_a_sparse_absence_a_deletion() {
    let b = Box::new();
    let at = clone_in_scratch(&b);
    fs::write(at.join("README"), "v1\n").unwrap();
    fs::write(at.join("other"), "o\n").unwrap();
    git(&at, &["add", "README", "other"]);
    git(&at, &["commit", "-q", "-m", "r"]);
    git(&at, &["update-index", "--skip-worktree", "other"]);
    fs::remove_file(at.join("other")).unwrap();
    fs::write(at.join("README"), "edited\n").unwrap();
    clear(&b.cfg, "u");
    let tracked = fs::read_to_string(kept(&b, "clone.1").join("tracked.patch")).unwrap();
    assert!(tracked.contains("+edited"), "{tracked}");
    assert!(!tracked.contains("deleted file"), "{tracked}");
}

// Conflict stages can't be kept in a patch: the scratch stays.
#[test]
fn keeping_work_stays_when_the_index_is_unmerged() {
    let b = Box::new();
    let at = clone_in_scratch(&b);
    fs::write(at.join("README"), "base\n").unwrap();
    git(&at, &["add", "README"]);
    git(&at, &["commit", "-q", "-m", "base"]);
    git(&at, &["checkout", "-q", "-b", "side"]);
    fs::write(at.join("README"), "side\n").unwrap();
    git(&at, &["commit", "-q", "-am", "side"]);
    git(&at, &["checkout", "-q", "-"]);
    fs::write(at.join("README"), "main\n").unwrap();
    git(&at, &["commit", "-q", "-am", "main"]);
    let merge = git_in(&at).args(["merge", "side"]).output().unwrap();
    assert!(!merge.status.success());
    // It stopped on the conflict, with both sides in the index, not before.
    assert!(
        !git_out(&at, &["ls-files", "-u"]).is_empty(),
        "{}",
        String::from_utf8_lossy(&merge.stderr)
    );
    fs::write(at.join("README"), "resolved by hand\n").unwrap();
    let e = keep_err(&b);
    assert!(e.contains("unmerged"), "{e}");
    assert!(at.exists());
}

// A submodule commit the saved top-level HEAD already names, with only an
// ordinary parent file changed, is kept with the parent.
#[test]
fn keeping_work_keeps_submodule_commits_a_saved_head_names() {
    let b = Box::new();
    let leaf = b.repo("leaf");
    let at = clone_in_scratch(&b);
    git(
        &at,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            leaf.to_str().unwrap(),
            "mod",
        ],
    );
    let m = at.join("mod");
    fs::write(m.join("unpublished.txt"), "u\n").unwrap();
    git(&m, &["add", "unpublished.txt"]);
    git(&m, &["commit", "-q", "-m", "unpublished"]);
    let sha = git_out(&m, &["rev-parse", "HEAD"]);
    git(&at, &["add", "mod"]);
    git(&at, &["commit", "-q", "-m", "record mod"]);
    fs::write(at.join("README"), "ordinary edit\n").unwrap();
    clear(&b.cfg, "u");
    let bundle = kept(&b, "clone-mod.1").join("base.bundle");
    assert!(bundle.exists(), "{:?}", left(&b.cfg));
    let heads = git_out(
        bundle.parent().unwrap(),
        &["bundle", "list-heads", "base.bundle"],
    );
    assert!(heads.contains(&sha));
}
