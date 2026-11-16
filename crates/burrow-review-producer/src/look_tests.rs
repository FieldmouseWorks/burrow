//! Queue fixtures use the shared reader with an explicit private boundary.
use super::*;
use std::os::unix::fs::MetadataExt;

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
    burrow_test::permissions(dir.path(), 0o770);
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
    burrow_test::permissions(&path, 0o660);
    assert!(
        queue
            .read_answer(&path, b"request")
            .unwrap_err()
            .to_string()
            .contains("writable")
    );
    burrow_test::permissions(&path, 0o640);
    burrow_test::permissions(&done, 0o770);
    assert!(
        queue
            .read_answer(&path, b"request")
            .unwrap_err()
            .to_string()
            .contains("writable")
    );
    burrow_test::permissions(&done, 0o700);
    let link = done.join("link.json");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    assert!(queue.read_answer(&link, b"request").is_err());
    // Check the writable ancestor itself from a controlled descriptor, so
    // an earlier host-owned directory cannot hide a missing mode check.
    assert!(
        burrow_review::answer::read_from(
            fs::File::open(dir.path()).unwrap(),
            Path::new("done/answer.json"),
            uid,
            b"request",
        )
        .unwrap_err()
        .to_string()
        .contains("writable")
    );
    // Without the fixture-only capability, Queue still checks from `/`.
    // A Nix build can have a foreign-owned ancestor before our writable
    // directory, so either check can reject this path first. The precise
    // mode failures are checked above through the controlled boundary.
    // Production cannot select that boundary.
    queue.answer_root = None;
    assert!(queue.read_answer(&path, b"request").is_err());
}
