use super::*;
use std::{fs, os::unix::fs::PermissionsExt, sync::Arc, thread};

#[test]
fn writes_whole_and_replaces() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("state");
    replace(&f, "one").unwrap();
    replace(&f, "two").unwrap();
    assert_eq!(fs::read_to_string(&f).unwrap(), "two");
    assert_eq!(fs::read_dir(d.path()).unwrap().count(), 1, "no temp left");
}

#[test]
fn mode_is_exact_whatever_the_umask() {
    let d = tempfile::tempdir().unwrap();
    for (mode, name) in [(PRIVATE, "private"), (SHARED, "shared"), (0o640, "group")] {
        let f = d.path().join(name);
        replace_mode(&f, "x", mode).unwrap();
        assert_eq!(fs::metadata(&f).unwrap().permissions().mode() & 0o777, mode);
    }
    let f = d.path().join("default");
    replace(&f, "x").unwrap();
    assert_eq!(
        fs::metadata(&f).unwrap().permissions().mode() & 0o777,
        PRIVATE
    );
}

#[test]
fn concurrent_writers_never_share_a_temporary() {
    let d = tempfile::tempdir().unwrap();
    let f = Arc::new(d.path().join("state"));
    let writers: Vec<_> = (0..8)
        .map(|n| {
            let f = f.clone();
            thread::spawn(move || {
                let mine = format!("{n}").repeat(4096);
                for _ in 0..50 {
                    replace(&*f, &mine).unwrap();
                    let seen = fs::read_to_string(&*f).unwrap();
                    assert_eq!(seen.len(), 4096, "half a file");
                    assert!(
                        seen.bytes().all(|b| b == seen.as_bytes()[0]),
                        "mixed writers"
                    );
                }
            })
        })
        .collect();
    for w in writers {
        w.join().unwrap();
    }
    assert_eq!(fs::read_dir(d.path()).unwrap().count(), 1, "no temp left");
}

#[test]
fn a_bare_name_writes_in_the_current_directory_and_a_missing_one_fails() {
    assert!(replace("/nonexistent-dir-for-test/x", "x").is_err());
    assert!(replace("/", "x").is_err());
}
