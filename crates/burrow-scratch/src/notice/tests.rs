// One notice per stuck entry however many sweeps see it, closed once it's
// gone (and forgotten only once HOST-notify shows it closed), a key of its
// own for each entry whatever its name holds, and no more than MOST open.

use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use super::{KEY, MOST, key, notice};

struct Box {
    _root: tempfile::TempDir,
    jobs: PathBuf,
    records: PathBuf,
    notify: PathBuf,
    said: PathBuf,
}

impl Box {
    /// HOST-notify as its callers see it: each call's first two words, a
    /// line each; `pending` answers 1 (closed) unless told otherwise.
    fn new() -> Box {
        let root = tempfile::tempdir().unwrap();
        let p = |n: &str| root.path().join(n);
        let (jobs, records, notify, said) = (p("jobs"), p("records"), p("notify"), p("said"));
        fs::create_dir(&jobs).unwrap();
        fs::create_dir(&records).unwrap();
        fs::write(
            &notify,
            format!(
                "#!/bin/sh\nif [ \"$1\" = pending ]; then [ -e {0}.open ] && exit 0; exit 1; fi\necho \"$1 $2\" >>{0}\n",
                said.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&notify, fs::Permissions::from_mode(0o755)).unwrap();
        Box {
            _root: root,
            jobs,
            records,
            notify,
            said,
        }
    }

    fn run(&self) -> Vec<String> {
        notice(
            &self.jobs,
            &self.records,
            "proj-scratch-sweep.service",
            Some(&self.notify),
        )
        .unwrap()
    }

    fn said(&self) -> String {
        fs::read_to_string(&self.said).unwrap_or_default()
    }
}

#[test]
fn one_notice_per_stuck_entry_closed_once_its_gone() {
    let b = Box::new();
    fs::create_dir(b.jobs.join(".stuck-j1-7")).unwrap();
    // Neither a set aside name nor a live job's is one.
    fs::create_dir(b.jobs.join(".gone-j2-8")).unwrap();
    fs::create_dir(b.jobs.join("j3")).unwrap();
    let k = key(&OsString::from(".stuck-j1-7"));
    assert!(k.starts_with("scratch-stuck-.stuck-j1-7-"), "{k}");
    assert_eq!(b.run(), [format!("ensure {k}")]);
    // Later sweeps that still see it ask nothing.
    assert!(b.run().is_empty());
    assert!(b.run().is_empty());
    assert_eq!(b.said(), format!("ensure {k}\n"));
    // Gone: closed, but kept while HOST-notify still has it open.
    fs::remove_dir(b.jobs.join(".stuck-j1-7")).unwrap();
    fs::write(b.said.with_extension("open"), "").unwrap();
    assert!(b.run().is_empty());
    assert!(b.records.join(&k).exists());
    fs::remove_file(b.said.with_extension("open")).unwrap();
    assert_eq!(b.run(), [format!("close {k}")]);
    assert!(!b.records.join(&k).exists());
    assert!(b.run().is_empty());
    assert_eq!(b.said(), format!("ensure {k}\nclose {k}\nclose {k}\n"));
}

#[test]
fn every_name_has_a_key_of_its_own_and_a_safe_one() {
    let a = key(&OsString::from(".stuck-a b"));
    let b = key(&OsString::from(".stuck-a_b"));
    let c = key(&OsString::from_vec(b".stuck-a\n\xff/..".to_vec()));
    assert_ne!(a, b);
    for k in [&a, &b, &c] {
        assert!(k.starts_with(KEY));
        assert!(
            k.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
            "{k}"
        );
    }
    let long = key(&OsString::from(format!(".stuck-{}", "x".repeat(500))));
    assert!(long.len() < 100, "{long}");
}

#[test]
fn no_more_than_most_open_at_once() {
    let b = Box::new();
    for i in 0..MOST + 3 {
        fs::create_dir(b.jobs.join(format!(".stuck-{i:02}"))).unwrap();
    }
    assert_eq!(b.run().len(), MOST);
    assert!(b.run().is_empty());
    // One clears: its close makes room for one more in the same run.
    fs::remove_dir(b.jobs.join(".stuck-00")).unwrap();
    let asked = b.run();
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert!(
        asked[0].starts_with("close ") && asked[1].starts_with("ensure "),
        "{asked:?}"
    );
    assert_eq!(fs::read_dir(&b.records).unwrap().count(), MOST);
}

#[test]
fn without_a_notifier_its_said_in_the_journal_once() {
    let b = Box::new();
    fs::create_dir(b.jobs.join(".stuck-j")).unwrap();
    let run = || notice(&b.jobs, &b.records, "u", None::<&Path>).unwrap();
    assert_eq!(run().len(), 1);
    assert!(run().is_empty());
    fs::remove_dir(b.jobs.join(".stuck-j")).unwrap();
    assert_eq!(run().len(), 1);
    assert_eq!(fs::read_dir(&b.records).unwrap().count(), 0);
}
