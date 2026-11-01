//! The drill's compare, on a snapshot and a restore made here: what passes,
//! and each way a restore can be wrong.

use std::ffi::OsString;
use std::fs::{self, File};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use burrow_backup::compare::{Verdict, compare};

struct Rig {
    dir: tempfile::TempDir,
}

impl Rig {
    /// A snapshot with a project in it (a file, a big one, a symlink, a
    /// dangling one, a directory), a path with a space in it, and a file the
    /// backup left out; the backup's listing; and a restore that's right.
    fn new() -> Rig {
        let r = Rig {
            dir: tempfile::tempdir().unwrap(),
        };
        let s = r.snap();
        for d in ["data", "data/proj", "data/proj/sub", "data/with space"] {
            fs::create_dir_all(s.join(d)).unwrap();
            fs::set_permissions(s.join(d), fs::Permissions::from_mode(0o755)).unwrap();
        }
        r.file(
            &s.join("data/proj/a.txt"),
            b"burrow-plaintext-marker\n",
            0o640,
        );
        let big: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        r.file(&s.join("data/proj/sub/big.bin"), &big, 0o644);
        symlink("a.txt", s.join("data/proj/link")).unwrap();
        symlink("/nowhere", s.join("data/proj/dangling")).unwrap();
        r.file(&s.join("data/with space/f"), b"spaced\n", 0o644);
        r.file(&s.join("data/proj/left-out"), b"x\n", 0o644);
        let listed = [
            "/data",
            "/data/proj",
            "/data/proj/a.txt",
            "/data/proj/sub",
            "/data/proj/sub/big.bin",
            "/data/proj/link",
            "/data/proj/dangling",
            "/data/with space",
            "/data/with space/f",
        ];
        r.list(&listed);
        for p in listed {
            r.restore(p);
        }
        r
    }

    fn snap(&self) -> PathBuf {
        self.dir.path().join("snap")
    }

    fn base(&self) -> PathBuf {
        self.dir.path().join("base")
    }

    fn tree(&self, p: &str) -> PathBuf {
        PathBuf::from(format!("{}/tree{p}", self.base().display()))
    }

    fn at(&self, p: &str) -> PathBuf {
        PathBuf::from(format!("{}{p}", self.snap().display()))
    }

    fn file(&self, path: &Path, bytes: &[u8], mode: u32) {
        fs::write(path, bytes).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
        // A time with nanoseconds in it, as a restore must keep.
        let t = SystemTime::UNIX_EPOCH + Duration::new(1_700_000_000, 123_456_789);
        File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(t)
            .unwrap();
    }

    fn list(&self, names: &[&str]) {
        fs::create_dir_all(self.base()).unwrap();
        let mut nul = Vec::new();
        for n in names {
            nul.extend_from_slice(n.as_bytes());
            nul.push(0);
        }
        fs::write(self.base().join("expected"), nul).unwrap();
    }

    /// `p` as restic restores it: the same kind, bytes, mode and mtime.
    fn restore(&self, p: &str) {
        let (from, to) = (self.at(p), self.tree(p));
        fs::create_dir_all(to.parent().unwrap()).unwrap();
        let m = fs::symlink_metadata(&from).unwrap();
        if m.file_type().is_symlink() {
            symlink(fs::read_link(&from).unwrap(), &to).unwrap();
        } else if m.is_dir() {
            fs::create_dir_all(&to).unwrap();
            fs::set_permissions(&to, m.permissions()).unwrap();
        } else {
            fs::copy(&from, &to).unwrap();
            fs::set_permissions(&to, m.permissions()).unwrap();
            File::options()
                .write(true)
                .open(&to)
                .unwrap()
                .set_modified(m.modified().unwrap())
                .unwrap();
        }
    }

    fn compare(&self, paths: &[&str]) -> Verdict {
        let paths: Vec<OsString> = paths.iter().map(OsString::from).collect();
        compare(&self.base(), &self.snap(), &paths).unwrap()
    }
}

fn passed(said: &[&str]) -> Verdict {
    Verdict {
        ok: true,
        said: said.iter().map(|s| s.to_string()).collect(),
    }
}

fn failed(said: &[&str]) -> Verdict {
    Verdict {
        ok: false,
        ..passed(said)
    }
}

#[test]
fn a_restore_that_is_the_snapshot_passes() {
    let r = Rig::new();
    assert_eq!(
        r.compare(&[
            "/data/proj",
            "/data/with space",
            "/data/proj/a.txt",
            "/data/proj/link",
            "/data/proj/dangling"
        ]),
        passed(&[
            "drill: /data/proj is what the snapshot had (6 entries)",
            "drill: /data/with space is what the snapshot had (2 entries)",
            "drill: /data/proj/a.txt is what the snapshot had (1 entries)",
            "drill: /data/proj/link is what the snapshot had (1 entries)",
            "drill: /data/proj/dangling is what the snapshot had (1 entries)",
        ])
    );
    // A trailing slash is the same path; `/` is all of it.
    assert_eq!(
        r.compare(&["/data/proj/", "/"]),
        passed(&[
            "drill: /data/proj is what the snapshot had (6 entries)",
            "drill: / is what the snapshot had (9 entries)",
        ])
    );
}

#[test]
fn a_path_the_backup_doesnt_have_fails() {
    let r = Rig::new();
    assert_eq!(
        r.compare(&["/data/none", "/data/pro", "/data/proj/a.txt"]),
        failed(&[
            "drill: /data/none isn't in the backup",
            "drill: /data/pro isn't in the backup",
            "drill: /data/proj/a.txt is what the snapshot had (1 entries)",
        ])
    );
}

#[test]
fn a_restore_missing_something_or_with_more_fails() {
    let r = Rig::new();
    fs::remove_file(r.tree("/data/proj/link")).unwrap();
    fs::write(r.tree("/data/proj/extra"), "x").unwrap();
    assert_eq!(
        r.compare(&["/data/proj"]),
        failed(&[
            "drill: restoring /data/proj didn't bring back what the backup holds (< only in the backup, > only restored):",
            "< /data/proj/link",
            "> /data/proj/extra",
        ])
    );
    // Nothing restored at all.
    fs::remove_dir_all(r.tree("/data/with space")).unwrap();
    assert_eq!(
        r.compare(&["/data/with space"]),
        failed(&[
            "drill: restoring /data/with space didn't bring back what the backup holds (< only in the backup, > only restored):",
            "< /data/with space",
            "< /data/with space/f",
        ])
    );
}

#[test]
fn what_differs_is_named_twenty_lines_at_most() {
    let r = Rig::new();
    let names: Vec<String> = (0..30).map(|i| format!("/data/many/{i:02}")).collect();
    let mut listed = vec!["/data/many"];
    listed.extend(names.iter().map(String::as_str));
    r.list(&listed);
    fs::create_dir_all(r.tree("/data/many")).unwrap();
    let v = r.compare(&["/data/many"]);
    assert!(!v.ok);
    assert_eq!(v.said.len(), 21, "{v:?}");
    assert_eq!(v.said[1], "< /data/many/00");
    assert_eq!(v.said[20], "< /data/many/19");
}

#[test]
fn other_bytes_fail_without_saying_them() {
    let r = Rig::new();
    // The same size and mtime, other bytes.
    let a = r.tree("/data/proj/a.txt");
    let m = fs::metadata(&a).unwrap();
    fs::write(&a, b"burrow-secret-text-xyz12").unwrap();
    File::options()
        .write(true)
        .open(&a)
        .unwrap()
        .set_modified(m.modified().unwrap())
        .unwrap();
    assert_eq!(fs::metadata(&a).unwrap().len(), m.len());
    let v = r.compare(&["/data/proj"]);
    assert_eq!(
        v,
        failed(&[
            "drill: /data/proj as restored isn't what the snapshot had:",
            "/data/proj/a.txt: its bytes differ",
        ])
    );
    let all = v.said.join("\n");
    assert!(
        !all.contains("secret") && !all.contains("plaintext"),
        "{all}"
    );
    // One byte in the middle of a big file, past the first read.
    let r = Rig::new();
    let big = r.tree("/data/proj/sub/big.bin");
    let mut bytes = fs::read(&big).unwrap();
    bytes[200_000] ^= 1;
    let m = fs::metadata(&big).unwrap();
    fs::write(&big, bytes).unwrap();
    File::options()
        .write(true)
        .open(&big)
        .unwrap()
        .set_modified(m.modified().unwrap())
        .unwrap();
    assert_eq!(
        r.compare(&["/data/proj/sub"]),
        failed(&[
            "drill: /data/proj/sub as restored isn't what the snapshot had:",
            "/data/proj/sub/big.bin: its bytes differ",
        ])
    );
}

#[test]
fn a_symlink_elsewhere_or_another_kind_fails() {
    let r = Rig::new();
    fs::remove_file(r.tree("/data/proj/link")).unwrap();
    symlink("sub", r.tree("/data/proj/link")).unwrap();
    fs::remove_file(r.tree("/data/proj/dangling")).unwrap();
    fs::write(r.tree("/data/proj/dangling"), "").unwrap();
    assert_eq!(
        r.compare(&["/data/proj"]),
        failed(&[
            "drill: /data/proj as restored isn't what the snapshot had:",
            "/data/proj/dangling: a symlink in the snapshot, a file restored",
            "/data/proj/link: the symlink's target differs",
        ])
    );
}

#[test]
fn a_fifo_restored_as_it_was_passes() {
    // A fifo has its kind and metadata to compare, and nothing else.
    let r = Rig::new();
    let mut expected = fs::read(r.base().join("expected")).unwrap();
    expected.extend_from_slice(b"/data/proj/pipe\0");
    fs::write(r.base().join("expected"), expected).unwrap();
    for at in [r.at("/data/proj/pipe"), r.tree("/data/proj/pipe")] {
        use rustix::fs::{CWD, Mode, mkfifoat};
        mkfifoat(CWD, &at, Mode::from_raw_mode(0o640)).unwrap();
        fs::set_permissions(&at, fs::Permissions::from_mode(0o640)).unwrap();
    }
    assert_eq!(
        r.compare(&["/data/proj"]),
        passed(&["drill: /data/proj is what the snapshot had (7 entries)"])
    );
    // Restored as a file, it's another kind.
    fs::remove_file(r.tree("/data/proj/pipe")).unwrap();
    r.file(&r.tree("/data/proj/pipe"), b"", 0o640);
    assert_eq!(
        r.compare(&["/data/proj"]),
        failed(&[
            "drill: /data/proj as restored isn't what the snapshot had:",
            "/data/proj/pipe: a fifo in the snapshot, a file restored",
        ])
    );
}

#[test]
fn what_the_snapshot_doesnt_have_fails() {
    let r = Rig::new();
    // In the backup and restored, and not in the snapshot.
    fs::remove_file(r.at("/data/with space/f")).unwrap();
    assert_eq!(
        r.compare(&["/data/with space"]),
        failed(&[
            "drill: /data/with space as restored isn't what the snapshot had:",
            "/data/with space/f: restored, but not in the snapshot",
        ])
    );
}

#[test]
fn other_metadata_fails() {
    let r = Rig::new();
    fs::set_permissions(
        r.tree("/data/proj/a.txt"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let mtime = SystemTime::UNIX_EPOCH + Duration::new(1_700_000_000, 123_456_000);
    File::options()
        .write(true)
        .open(r.tree("/data/proj/sub/big.bin"))
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    fs::set_permissions(r.tree("/data/proj/sub"), fs::Permissions::from_mode(0o700)).unwrap();
    let v = r.compare(&["/data/proj"]);
    let (uid, gid) = {
        use std::os::unix::fs::MetadataExt;
        let m = fs::metadata(r.at("/data/proj/a.txt")).unwrap();
        (m.uid(), m.gid())
    };
    assert_eq!(
        v,
        failed(&[
            "drill: the metadata of /data/proj as restored isn't what the snapshot had (< snapshot, > restored):",
            &format!("< /data/proj/a.txt f 640 {uid} {gid} 24 1700000000.123456789"),
            &format!("> /data/proj/a.txt f 600 {uid} {gid} 24 1700000000.123456789"),
            &format!("< /data/proj/sub d 755 {uid} {gid}"),
            &format!("> /data/proj/sub d 700 {uid} {gid}"),
            &format!("< /data/proj/sub/big.bin f 644 {uid} {gid} 300000 1700000000.123456789"),
            &format!("> /data/proj/sub/big.bin f 644 {uid} {gid} 300000 1700000000.123456000"),
        ])
    );
}

#[test]
fn a_listing_it_cant_read_is_an_error() {
    let r = Rig::new();
    fs::remove_file(r.base().join("expected")).unwrap();
    let e = compare(&r.base(), &r.snap(), &[OsString::from("/data/proj")]).unwrap_err();
    assert!(
        e.ends_with("/base/expected: No such file or directory (os error 2)"),
        "{e}"
    );
}
