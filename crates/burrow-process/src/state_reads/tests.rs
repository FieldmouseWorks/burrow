use super::*;
use std::{fs, os::unix::fs::symlink};
#[test]
fn pinned_directories_reject_escape_paths_links_and_special_files() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fs::create_dir(root.join("job")).unwrap();
    fs::write(root.join("secret"), "confidential text").unwrap();
    let job = Dir::open(&root.join("job")).unwrap();
    symlink(root.join("secret"), root.join("job/role")).unwrap();
    symlink(root.join("job"), root.join("alias")).unwrap();
    assert!(job.text("role").is_err());
    assert!(job.read_lock("role").is_err());
    assert!(Dir::open(&root.join("alias")).is_err());
    assert!(job.file(Path::new("../secret")).is_err());
    assert!(job.file(&root.join("secret")).is_err());
    assert!(open(&root.join("job/../secret")).is_err());
    rustix::fs::mkfifoat(rustix::fs::CWD, root.join("job/fifo"), Mode::RUSR).unwrap();
    assert!(job.text("fifo").is_err());
    assert!(job.read_lock("fifo").is_err());
    assert!(open(Path::new("/dev/zero")).is_err());
    assert!(job.file(Path::new(".")).is_err());
    // Replace the entire job directory after pinning it: lookups retain the
    // original directory, not the symlink installed at its old pathname.
    fs::write(root.join("job/value"), "public").unwrap();
    fs::rename(root.join("job"), root.join("kept")).unwrap();
    symlink(root, root.join("job")).unwrap();
    assert_eq!(job.text("value").unwrap(), "public");
    assert!(job.text("secret").is_err());
    assert!(Dir::open(&root.join("job")).is_err());
}
#[test]
fn pinned_regular_files_survive_replacement_and_first_observations_are_immutable() {
    use std::io::Read;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fs::write(root.join("stream"), "public").unwrap();
    fs::write(root.join("secret"), "confidential text").unwrap();
    let dir = Dir::open(root).unwrap();
    let mut pinned = dir.file(Path::new("stream")).unwrap();
    fs::remove_file(root.join("stream")).unwrap();
    symlink(root.join("secret"), root.join("stream")).unwrap();
    let mut text = String::new();
    pinned.read_to_string(&mut text).unwrap();
    assert_eq!(text, "public");
    assert!(dir.file(Path::new("stream")).is_err());
    assert!(dir.append_file("stream").is_err());
    assert_eq!(
        fs::read_to_string(root.join("secret")).unwrap(),
        "confidential text"
    );
    dir.create("started", b"100").unwrap();
    assert_eq!(
        dir.create("started", b"200").unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(dir.text("started").unwrap(), "100");
}
