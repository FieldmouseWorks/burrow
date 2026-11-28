// What a sweep takes (only the set aside and stuck names, whole, whatever is
// in them), what it leaves (a link's target, every other name), and what it
// does with what it can't remove (renamed out of the watched name, the rest
// still removed, tried again next time). Mounts and other users' files need
// root: tests/proj-scratch.nix has those.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use super::{Sweep, sweep};

fn jobs() -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let jobs = root.path().join("jobs");
    fs::create_dir(&jobs).unwrap();
    (root, jobs)
}

fn names(jobs: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(jobs)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

fn tree(at: &Path) {
    fs::create_dir_all(at.join("web/target/debug")).unwrap();
    fs::write(at.join("web/target/debug/lib.rlib"), "x").unwrap();
    fs::write(at.join("web/.git"), "gitdir: /nowhere\n").unwrap();
}

/// Whether this runs as root, whom a read-only dir doesn't stop.
fn root() -> bool {
    let d = tempfile::tempdir().unwrap();
    burrow_test::permissions(d.path(), 0o555);
    let wrote = fs::write(d.path().join("f"), "").is_ok();
    burrow_test::permissions(d.path(), 0o755);
    wrote
}

#[test]
fn takes_only_what_was_set_aside_and_all_of_it() {
    let (_root, jobs) = jobs();
    tree(&jobs.join(".gone-a-1"));
    fs::write(jobs.join(".gone-b-2"), "a file").unwrap();
    tree(&jobs.join(".stuck-c-3"));
    // A job's live scratch, and anything else beside them.
    tree(&jobs.join("d"));
    fs::create_dir(jobs.join(".gonee")).unwrap();
    fs::create_dir(jobs.join("x.gone-y")).unwrap();
    let done = sweep(&jobs).unwrap();
    assert_eq!(done.removed, [".gone-a-1", ".gone-b-2", ".stuck-c-3"]);
    assert!(
        done.stuck.is_empty() && done.still.is_empty() && done.held.is_empty(),
        "{done:?}"
    );
    assert_eq!(names(&jobs), [".gonee", "d", "x.gone-y"]);
    assert!(jobs.join("d/web/target/debug/lib.rlib").exists());
    // Nothing left: nothing to do.
    assert_eq!(sweep(&jobs).unwrap(), Sweep::default());
}

#[test]
fn a_link_goes_and_what_it_names_stays() {
    let (root, jobs) = jobs();
    let target = root.path().join("target");
    tree(&target);
    // Set aside as a link, and a link inside a tree set aside, to the dir and
    // into it.
    symlink(&target, jobs.join(".gone-link-1")).unwrap();
    fs::create_dir(jobs.join(".gone-t-2")).unwrap();
    symlink(&target, jobs.join(".gone-t-2/to-dir")).unwrap();
    symlink(target.join("web/target"), jobs.join(".gone-t-2/to-sub")).unwrap();
    let done = sweep(&jobs).unwrap();
    assert_eq!(done.removed, [".gone-link-1", ".gone-t-2"]);
    assert_eq!(names(&jobs), Vec::<String>::new());
    assert_eq!(
        fs::read_to_string(target.join("web/target/debug/lib.rlib")).unwrap(),
        "x"
    );
}

#[test]
fn what_it_cant_remove_goes_stuck_and_the_rest_still_goes() {
    if root() {
        eprintln!("skipped: root removes what a read-only dir holds");
        return;
    }
    let (_root, jobs) = jobs();
    // A dir its user can't empty (a build's read-only output, to a user who
    // isn't root), between two that go; and a stuck name already taken.
    tree(&jobs.join(".gone-a-1"));
    tree(&jobs.join(".gone-ro-2"));
    burrow_test::permissions(jobs.join(".gone-ro-2/web"), 0o555);
    tree(&jobs.join(".gone-z-3"));
    fs::create_dir(jobs.join(".stuck-ro-2")).unwrap();
    let done = sweep(&jobs).unwrap();
    assert_eq!(done.removed, [".gone-a-1", ".gone-z-3", ".stuck-ro-2"]);
    assert_eq!(done.stuck.len(), 1, "{done:?}");
    assert_eq!(done.stuck[0].0, ".stuck-ro-2.1");
    assert!(
        done.stuck[0].1.contains(".gone-ro-2/web/"),
        "{}",
        done.stuck[0].1
    );
    // Out of the watched name, with what could go gone.
    assert_eq!(names(&jobs), [".stuck-ro-2.1"]);
    // Stuck is out of the watched name and noticed: not a failed sweep.
    assert!(!done.failed());
    assert_eq!(
        fs::read_dir(jobs.join(".stuck-ro-2.1/web"))
            .unwrap()
            .count(),
        2
    );
    // Still stuck next time, and gone the first time after it's fixed.
    let again = sweep(&jobs).unwrap();
    assert!(
        again.removed.is_empty() && again.stuck.is_empty(),
        "{again:?}"
    );
    assert_eq!(again.still.len(), 1);
    burrow_test::permissions(jobs.join(".stuck-ro-2.1/web"), 0o755);
    assert_eq!(sweep(&jobs).unwrap().removed, [".stuck-ro-2.1"]);
    assert_eq!(names(&jobs), Vec::<String>::new());
}

#[test]
fn a_jobs_dir_that_is_a_link_is_refused() {
    let (root, jobs) = jobs();
    tree(&jobs.join(".gone-a-1"));
    let link = root.path().join("link");
    symlink(&jobs, &link).unwrap();
    assert!(sweep(&link).is_err());
    assert!(sweep(&root.path().join("jobs/../jobs")).is_err());
    assert_eq!(names(&jobs), [".gone-a-1"]);
}

// Only what it leaves under the watched name fails it: nothing else notices.
#[test]
fn a_sweep_fails_only_on_what_it_leaves_under_the_watched_name() {
    let why = || "why".to_owned();
    let mut done = Sweep {
        removed: vec![".gone-a".into()],
        stuck: vec![(".stuck-b".into(), why())],
        still: vec![(".stuck-c".into(), why())],
        held: vec![],
    };
    assert!(!done.failed());
    done.held.push((".gone-d".into(), why()));
    assert!(done.failed());
}

#[test]
fn a_max_length_entry_that_cant_be_removed_still_goes_stuck() {
    if root() {
        eprintln!("skipped: root removes what a read-only dir holds");
        return;
    }
    let (_root, jobs) = jobs();
    let rest = "a".repeat(255 - crate::GONE.len());
    let gone = format!("{}{rest}", crate::GONE);
    assert_eq!(gone.len(), 255);
    tree(&jobs.join(&gone));
    burrow_test::permissions(jobs.join(&gone).join("web"), 0o555);
    // Occupy the first truncated stuck slot too, so collision suffixing (.1) is exercised.
    let taken = format!("{}{}", crate::STUCK, &rest[..255 - crate::STUCK.len()]);
    assert_eq!(taken.len(), 255);
    tree(&jobs.join(&taken));
    burrow_test::permissions(jobs.join(&taken).join("web"), 0o555);

    let done = sweep(&jobs).unwrap();
    assert!(done.held.is_empty(), "held: {:?}", done.held);
    assert_eq!(done.stuck.len(), 1, "{done:?}");
    let stuck_name = &done.stuck[0].0;
    assert!(stuck_name.len() <= 255, "len={}", stuck_name.len());
    assert!(
        stuck_name.to_string_lossy().ends_with(".1"),
        "{stuck_name:?}"
    );

    // Restore permissions so TempDir cleans up cleanly.
    burrow_test::permissions(jobs.join(stuck_name).join("web"), 0o755);
    burrow_test::permissions(jobs.join(&taken).join("web"), 0o755);
}
