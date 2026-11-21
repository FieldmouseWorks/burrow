use super::*;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::PathBuf;

thread_local! {
    /// Every file and directory [`durable`] flushed, by device and inode:
    /// a test's carry runs on its own thread.
    pub(super) static SYNCED: std::cell::RefCell<std::collections::HashSet<(u64, u64)>> =
        Default::default();
}

/// Each entry under `dir`, `dir` too, that the carry left unflushed.
fn unsynced(dir: &Path) -> Vec<PathBuf> {
    let mut left = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(path) = todo.pop() {
        let meta = std::fs::symlink_metadata(&path).unwrap();
        if !SYNCED.with(|s| s.borrow().contains(&(meta.dev(), meta.ino()))) {
            left.push(path.clone());
        }
        if meta.is_dir() {
            for entry in std::fs::read_dir(&path).unwrap() {
                todo.push(entry.unwrap().path());
            }
        }
    }
    left
}

/// The test's own user: the state is handed to it, which a user may do.
fn me() -> Owner {
    Owner {
        uid: rustix::process::getuid().as_raw(),
        gid: rustix::process::getgid().as_raw(),
    }
}

/// A machine's two places for the flow's state, neither there yet: the
/// ledger's `data` and the controller's own, beside a file outside both.
fn places() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let ledger = root.path().join("conary");
    std::fs::create_dir(&ledger).unwrap();
    let outside = root.path().join("secret");
    std::fs::write(&outside, "secret").unwrap();
    let data = root.path().join("flow");
    (root, data, ledger.join("data"), outside)
}

/// The flow's state as Conary leaves it: a PR's record, the PR list, a
/// pause and its pass's lock, and conaryd's queue beside them.
const STATE: [(&str, &str); 5] = [
    (
        "flow/sam/plain/5.json",
        r#"{"memory":{"asked":["abc",100]}}"#,
    ),
    ("flow/sam/plain/open", r#"{"at":1,"prs":[5]}"#),
    ("flow/sam/plain/pause", r#"{"reason":"held"}"#),
    ("flow/sam/plain/lock", ""),
    ("events.json", "[]"),
];

fn write_state(dir: &Path) {
    for (name, body) in STATE {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, body).unwrap();
    }
}

fn holds_state(dir: &Path) -> bool {
    STATE
        .iter()
        .all(|(name, body)| std::fs::read_to_string(dir.join(name)).ok().as_deref() == Some(*body))
}

fn mode(path: &Path) -> u32 {
    std::fs::symlink_metadata(path)
        .unwrap()
        .permissions()
        .mode()
        & 0o7777
}

fn inode(path: &Path) -> u64 {
    std::fs::symlink_metadata(path).unwrap().ino()
}

#[test]
fn state_where_it_was_kept_is_carried_and_gone_from_there() {
    let (_root, data, other, _outside) = places();
    write_state(&other);
    let before = inode(&other.join("flow/sam/plain/5.json"));
    let mtime = std::fs::metadata(other.join("flow/sam/plain/5.json"))
        .unwrap()
        .mtime();
    assert_eq!(carry(&data, &other, me()).unwrap(), Carried::Moved);
    assert!(holds_state(&data));
    assert!(!other.exists(), "what was kept before is gone");
    assert_eq!(mode(&data), 0o700);
    // Fresh files: a descriptor the old user held leads to none of them.
    assert_ne!(inode(&data.join("flow/sam/plain/5.json")), before);
    assert_eq!(
        std::fs::metadata(data.join("flow/sam/plain/5.json"))
            .unwrap()
            .mtime(),
        mtime
    );
    assert!(!data.parent().unwrap().join(".burrow-flow-carry").exists());
    // Carried once: the next run keeps it.
    assert_eq!(carry(&data, &other, me()).unwrap(), Carried::Kept);
    assert!(holds_state(&data));
}

#[test]
fn an_empty_directory_holds_no_state() {
    // As tmpfiles or a unit made it before the first run.
    let (_root, data, other, _outside) = places();
    std::fs::create_dir(&data).unwrap();
    write_state(&other);
    assert_eq!(carry(&data, &other, me()).unwrap(), Carried::Moved);
    assert!(holds_state(&data));
    // And one left where it was kept goes.
    std::fs::create_dir(&other).unwrap();
    assert_eq!(carry(&data, &other, me()).unwrap(), Carried::Kept);
    assert!(!other.exists());
}

#[test]
fn two_histories_are_refused_and_left_as_they_were() {
    let (_root, data, other, _outside) = places();
    write_state(&other);
    std::fs::create_dir_all(data.join("flow/sam/plain")).unwrap();
    std::fs::write(data.join("flow/sam/plain/7.json"), "{}").unwrap();
    let e = carry(&data, &other, me()).unwrap_err();
    assert!(matches!(e, Refused::Refused(_)), "{e}");
    assert!(holds_state(&other));
    assert!(data.join("flow/sam/plain/7.json").exists());
    assert!(!data.join("flow/sam/plain/5.json").exists());
}

#[test]
fn a_link_in_the_state_is_refused_and_what_it_leads_to_untouched() {
    let (_root, data, other, outside) = places();
    write_state(&other);
    symlink(&outside, other.join("flow/sam/plain/6.json")).unwrap();
    let e = carry(&data, &other, me()).unwrap_err();
    assert!(matches!(e, Refused::Refused(_)), "{e}");
    assert!(e.to_string().contains("6.json: a link"), "{e}");
    assert!(!data.exists(), "nothing put in place");
    assert!(!data.parent().unwrap().join(".burrow-flow-carry").exists());
    assert!(holds_state(&other));
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "secret");
}

#[test]
fn a_link_for_the_directory_itself_is_refused() {
    let (_root, data, other, _outside) = places();
    let elsewhere = data.parent().unwrap().join("elsewhere");
    write_state(&elsewhere);
    symlink(&elsewhere, &other).unwrap();
    assert!(matches!(
        carry(&data, &other, me()),
        Err(Refused::Refused(_))
    ));
    assert!(!data.exists());
    // Where it's kept now, a link is never the state.
    std::fs::remove_file(&other).unwrap();
    symlink(&elsewhere, &data).unwrap();
    assert!(matches!(
        carry(&data, &other, me()),
        Err(Refused::Refused(_))
    ));
    assert!(holds_state(&elsewhere));
    // Beside state where it's kept now, a link where it was is let be.
    std::fs::remove_file(&data).unwrap();
    write_state(&data);
    symlink(&elsewhere, &other).unwrap();
    burrow_test::permissions(&data, 0o700);
    assert_eq!(carry(&data, &other, me()).unwrap(), Carried::Kept);
}

#[test]
fn a_file_linked_from_elsewhere_is_refused() {
    let (_root, data, other, outside) = places();
    write_state(&other);
    std::fs::hard_link(&outside, other.join("flow/sam/plain/6.json")).unwrap();
    let e = carry(&data, &other, me()).unwrap_err();
    assert!(e.to_string().contains("linked from elsewhere"), "{e}");
    assert!(!data.exists());
    assert!(holds_state(&other));
}

#[test]
fn a_locked_file_waits_for_its_process() {
    // A worker of the flow's, outliving conaryd, holds its `.running`.
    let (_root, data, other, _outside) = places();
    write_state(&other);
    let running = other.join("flow/sam/plain/5-1.running");
    std::fs::write(&running, "").unwrap();
    let held = std::fs::File::open(&running).unwrap();
    held.try_lock().unwrap();
    let e = carry(&data, &other, me()).unwrap_err();
    assert!(matches!(e, Refused::Busy(_)), "{e}");
    assert!(!data.exists());
    assert!(holds_state(&other));
    drop(held);
    assert_eq!(carry(&data, &other, me()).unwrap(), Carried::Moved);
    assert!(holds_state(&data));
}

#[test]
fn with_no_state_anywhere_an_empty_directory_waits() {
    let (_root, data, other, _outside) = places();
    assert_eq!(carry(&data, &other, me()).unwrap(), Carried::Made);
    assert_eq!(mode(&data), 0o700);
    assert_eq!(std::fs::read_dir(&data).unwrap().count(), 0);
    assert_eq!(carry(&data, &other, me()).unwrap(), Carried::Kept);
    // Where it was kept may be gone with its directory.
    std::fs::remove_dir(&data).unwrap();
    let gone = data.parent().unwrap().join("no-ledger/data");
    assert_eq!(carry(&data, &gone, me()).unwrap(), Carried::Made);
}

#[test]
fn state_already_its_owners_is_kept_and_only_narrowed() {
    // The ledger's `data` as the flow kept it before this ran: its owner's,
    // but a directory others may enter. Nothing is copied, so a worker
    // still holding its lock holds nothing up.
    let (_root, data, other, _outside) = places();
    write_state(&data);
    burrow_test::permissions(&data, 0o755);
    let before = inode(&data.join("flow/sam/plain/5.json"));
    let held = std::fs::File::open(data.join("flow/sam/plain/lock")).unwrap();
    held.try_lock().unwrap();
    assert_eq!(carry(&data, &other, me()).unwrap(), Carried::Kept);
    assert!(holds_state(&data));
    assert_eq!(mode(&data), 0o700);
    assert_eq!(inode(&data.join("flow/sam/plain/5.json")), before);
}

#[test]
fn a_removal_cut_short_leaves_no_second_history() {
    // What a carry put aside and couldn't finish removing.
    let (_root, data, other, _outside) = places();
    write_state(&data);
    burrow_test::permissions(&data, 0o700);
    let aside = other.parent().unwrap().join(".burrow-flow-carry");
    write_state(&aside);
    assert_eq!(carry(&data, &other, me()).unwrap(), Carried::Kept);
    // And the next carry clears it before it puts its own aside.
    std::fs::remove_dir_all(&data).unwrap();
    write_state(&other);
    assert_eq!(carry(&data, &other, me()).unwrap(), Carried::Moved);
    assert!(!aside.exists());
    assert!(!other.exists());
    assert!(holds_state(&data));
}

#[test]
fn a_copy_cut_short_is_cleared_first() {
    let (_root, data, other, outside) = places();
    let staging = data.parent().unwrap().join(".burrow-flow-carry");
    // What a run cut short left, and a link someone put there instead.
    std::fs::create_dir_all(staging.join("flow")).unwrap();
    write_state(&other);
    assert_eq!(carry(&data, &other, me()).unwrap(), Carried::Moved);
    assert!(!staging.exists());
    std::fs::remove_dir_all(&data).unwrap();
    write_state(&other);
    symlink(&outside, &staging).unwrap();
    assert_eq!(carry(&data, &other, me()).unwrap(), Carried::Moved);
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "secret");
}

#[test]
fn every_directory_and_file_carried_is_on_disk_before_the_old_goes() {
    let (_root, data, other, _outside) = places();
    write_state(&other);
    SYNCED.with(|s| s.borrow_mut().clear());
    assert_eq!(carry(&data, &other, me()).unwrap(), Carried::Moved);
    assert!(!other.exists());
    assert_eq!(unsynced(&data), Vec::<PathBuf>::new());
}

#[test]
fn the_old_copy_is_gone_from_disk_once_carried() {
    // Where it was kept is flushed once it's removed, so a crash can't
    // bring it back beside the carried copy as a second history.
    let (_root, data, other, _outside) = places();
    write_state(&other);
    let parent = other.parent().unwrap().to_path_buf();
    SYNCED.with(|s| s.borrow_mut().clear());
    assert_eq!(carry(&data, &other, me()).unwrap(), Carried::Moved);
    assert!(!other.exists());
    assert_eq!(unsynced(&parent), Vec::<PathBuf>::new());
}

#[test]
fn every_directory_and_file_handed_in_place_is_on_disk() {
    // A controller changed: [`carry`] copies in place what another user
    // owns, which a test can't make as its own user, so the copy it runs
    // is run here directly.
    let (_root, data, _other, _outside) = places();
    write_state(&data);
    let before = inode(&data.join("flow/sam/plain/5.json"));
    let (dp, dn) = place(&data).unwrap().unwrap();
    SYNCED.with(|s| s.borrow_mut().clear());
    copy(&dp, &dn, &dp, &dn, me(), "data").unwrap();
    assert!(holds_state(&data));
    assert_eq!(mode(&data), 0o700);
    assert_ne!(inode(&data.join("flow/sam/plain/5.json")), before);
    assert!(!data.parent().unwrap().join(".burrow-flow-carry").exists());
    assert_eq!(unsynced(&data), Vec::<PathBuf>::new());
}
