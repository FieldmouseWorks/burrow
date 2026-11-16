//! Queue fixtures use the shared reader with an explicit private boundary.
use super::*;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

#[test]
fn a_private_queue_boundary_preserves_answer_checks() {
    let dir = tempfile::tempdir().unwrap();
    let done = dir.path().join("done");
    fs::create_dir(&done).unwrap();
    let path = done.join("answer.json");
    let uid = fs::metadata(&done).unwrap().uid();
    let mut queue = Queue {
        queue: dir.path().join("queue"),
        done: done.clone(),
        every: Duration::from_millis(10),
        reviewer_uid: uid,
        answer_root: Some(fs::File::open(&done).unwrap()),
    };
    burrow_review::answer::save(
        &path,
        &serde_json::json!({
            "digest": burrow_review::answer::digest(b"request")
        }),
    )
    .unwrap();
    // A normal temporary checkout or Nix build can have this ancestor.
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o770)).unwrap();
    assert!(queue.read_answer(&path, b"request").is_ok());
    queue.reviewer_uid = uid + 1;
    assert!(
        queue
            .read_answer(&path, b"request")
            .unwrap_err()
            .to_string()
            .contains("owner")
    );
    queue.reviewer_uid = uid;
    assert!(
        queue
            .read_answer(&path, b"other")
            .unwrap_err()
            .to_string()
            .contains("digest")
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o660)).unwrap();
    assert!(
        queue
            .read_answer(&path, b"request")
            .unwrap_err()
            .to_string()
            .contains("writable")
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
    fs::set_permissions(&done, fs::Permissions::from_mode(0o770)).unwrap();
    assert!(
        queue
            .read_answer(&path, b"request")
            .unwrap_err()
            .to_string()
            .contains("writable")
    );
    fs::set_permissions(&done, fs::Permissions::from_mode(0o700)).unwrap();
    let link = done.join("link.json");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    assert!(queue.read_answer(&link, b"request").is_err());
    // Without the fixture-only capability, Queue still checks from `/` and
    // rejects the same writable ancestor. Production cannot select a root.
    queue.answer_root = None;
    assert!(
        queue
            .read_answer(&path, b"request")
            .unwrap_err()
            .to_string()
            .contains("writable")
    );
}
