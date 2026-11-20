use super::*;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;

/// The test's own uid. The ledgers on disk name only it, so the tests that
/// set ACLs need no other user; they run in checks.ledger-controller's VM,
/// since a Nix build's seccomp filter refuses every setxattr.
const ME: fn() -> u32 = || rustix::process::getuid().as_raw();
/// Controllers for [`want`] alone, which touches no file.
const OWNER: u32 = 1000;
const FLOW: u32 = 4242;
const MOVED: u32 = 4343;

/// A ledger as Conary's writer leaves one, beside a file outside it.
fn ledger() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("conary");
    std::fs::create_dir(&dir).unwrap();
    for (name, mode) in [
        ("key", 0o600),
        ("key.pub", 0o644),
        ("ops.jsonl", 0o644),
        ("lock", 0o644),
        ("merge-bases", 0o644),
        ("anchor", 0o644),
    ] {
        let path = dir.join(name);
        std::fs::write(&path, name).unwrap();
        burrow_test::permissions(&path, mode);
    }
    std::fs::create_dir(dir.join("etags")).unwrap();
    let outside = root.path().join("secret");
    std::fs::write(&outside, "secret").unwrap();
    burrow_test::permissions(&outside, 0o600);
    (root, dir, outside)
}

fn acl(path: &Path, name: &str) -> Option<Acl> {
    let path = path.to_str().unwrap();
    let mut buf = vec![0u8; 65536];
    match fs::lgetxattr(path, name, &mut buf[..]) {
        Ok(n) => Some(Acl::decode(&buf[..n]).unwrap()),
        Err(Errno::NODATA) => None,
        Err(e) => panic!("{path}: {e}"),
    }
}

fn mode(path: &Path) -> u32 {
    std::fs::symlink_metadata(path)
        .unwrap()
        .permissions()
        .mode()
        & 0o7777
}

fn run(dir: &Path, controller: Option<u32>) {
    let mut said = Vec::new();
    let failed = set(
        dir,
        &Access {
            owner: ME(),
            controller,
        },
        &mut |l| said.push(l),
    );
    assert!(!failed, "{said:?}");
}

fn entries(acl: &Acl) -> Vec<(u16, u32, u16)> {
    acl.0.iter().map(|e| (e.tag, e.id, e.perm)).collect()
}

/// Every uid an ACL in the ledger names.
fn named(dir: &Path) -> Vec<u32> {
    let mut paths = vec![dir.to_path_buf()];
    paths.extend(std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()));
    paths
        .iter()
        .filter(|p| !p.is_symlink())
        .flat_map(|p| [acl(p, ACCESS), acl(p, DEFAULT)])
        .flatten()
        .flat_map(|a| a.0)
        .filter(|e| matches!(e.tag, USER | GROUP))
        .map(|e| e.id)
        .collect()
}

#[test]
fn an_acl_reads_back_as_written() {
    let a = Base {
        user: 6,
        group: 4,
        other: 0,
    }
    .with(&[(FLOW, 6), (7, 4)], 6);
    assert_eq!(Acl::decode(&a.encode()), Some(a.clone()));
    assert_eq!(entries(&a)[1], (USER, 7, 4), "named users sorted by id");
    assert_eq!(Acl::decode(&[2, 0, 0, 0, 1]), None);
    assert_eq!(Acl::decode(&[1, 0, 0, 0]), None);
}

#[test]
fn uids_come_from_passwd() {
    let passwd = "root:x:0:0::/root:/bin/sh\ndev:x:1000:100::/home/dev:/bin/sh\nconary-flow:x:991:990::/var/empty:/bin/false\n";
    assert_eq!(uid_of(passwd, "dev"), Some(1000));
    assert_eq!(uid_of(passwd, "conary-flow"), Some(991));
    assert_eq!(uid_of(passwd, "conary"), None);
}

fn wants(controller: Option<u32>, what: What, mode: u32, now: Option<&Acl>) -> Want {
    want(
        &Access {
            owner: OWNER,
            controller,
        },
        what,
        mode,
        now,
        None,
    )
}

#[test]
fn the_controller_reads_the_key_writes_the_log_and_no_one_else_gets_either() {
    let flow = Some(FLOW);
    let key = wants(flow, What::File(b"key"), 0o100600, None);
    let key = entries(&key.access.unwrap());
    assert_eq!(
        key,
        [
            (USER_OBJ, NO_ID, 6),
            (USER, FLOW, 4),
            (GROUP_OBJ, NO_ID, 0),
            (MASK, NO_ID, 4),
            (OTHER, NO_ID, 0)
        ]
    );
    // However the key was made: its owning group gets nothing.
    let made = wants(flow, What::File(b"key"), 0o100660, None);
    assert_eq!(entries(&made.access.unwrap()), key);
    for w in WRITABLE {
        let a = wants(flow, What::File(w.as_bytes()), 0o100644, None);
        let expect = [
            (USER_OBJ, NO_ID, 6),
            (USER, FLOW, 6),
            (GROUP_OBJ, NO_ID, 4),
            (MASK, NO_ID, 6),
            (OTHER, NO_ID, 4),
        ];
        assert_eq!(entries(&a.access.unwrap()), expect, "{w}");
    }
    let ledger = wants(flow, What::Ledger, 0o40755, None);
    let access = entries(&ledger.access.unwrap());
    assert_eq!(
        access,
        [
            (USER_OBJ, NO_ID, 7),
            (USER, FLOW, 7),
            (GROUP_OBJ, NO_ID, 5),
            (MASK, NO_ID, 7),
            (OTHER, NO_ID, 5)
        ]
    );
    let default = entries(&ledger.default.unwrap());
    assert_eq!(
        default,
        [
            (USER_OBJ, NO_ID, 7),
            (USER, OWNER, 7),
            (USER, FLOW, 7),
            (GROUP_OBJ, NO_ID, 5),
            (MASK, NO_ID, 7),
            (OTHER, NO_ID, 5)
        ]
    );
    // etags names no one, and a file nothing gave an ACL keeps none.
    let none = Want {
        access: None,
        default: None,
    };
    let etags = Base {
        user: 7,
        group: 0,
        other: 0,
    }
    .with(&[(FLOW, 7)], 7);
    assert_eq!(wants(flow, What::Dir, 0o40700, Some(&etags)), none);
    assert_eq!(wants(flow, What::File(b"key.pub"), 0o100644, None), none);
}

#[test]
fn a_controller_changed_or_turned_off_keeps_nothing() {
    // Each entry as the first controller left it, and a file its default
    // entries gave it, with a named group someone added.
    let mut made = Base {
        user: 6,
        group: 4,
        other: 4,
    }
    .with(&[(FLOW, 7)], 4);
    made.0.push(Entry {
        tag: GROUP,
        id: 77,
        perm: 4,
    });
    made.0.sort();
    let whats = [
        (What::Ledger, 0o40775),
        (What::File(b"key"), 0o100640),
        (What::File(b"ops.jsonl"), 0o100664),
        (What::File(b"key.pub"), 0o100644),
    ];
    for (what, mode) in whats {
        let first = match what {
            What::File(b"key.pub") => Some(made.clone()),
            _ => wants(Some(FLOW), what, mode, None).access,
        };
        let moved = wants(Some(MOVED), what, mode, first.as_ref());
        let ids: Vec<u32> = [&moved.access, &moved.default]
            .into_iter()
            .flatten()
            .flat_map(|a| {
                a.0.iter()
                    .filter(|e| matches!(e.tag, USER | GROUP))
                    .map(|e| e.id)
            })
            .collect();
        assert!(
            !ids.contains(&FLOW) && !ids.contains(&77),
            "{what:?}: {ids:?}"
        );
        assert_eq!(
            wants(None, what, mode, first.as_ref()),
            Want {
                access: None,
                default: None
            },
            "{what:?}"
        );
    }
    // A file the new controller's default entries made keeps its entry.
    let mine = Base {
        user: 6,
        group: 4,
        other: 4,
    }
    .with(&[(MOVED, 7)], 4);
    assert_eq!(
        wants(Some(MOVED), What::File(b"key.pub"), 0o100644, Some(&mine)).access,
        Some(mine)
    );
}

#[test]
#[ignore = "sets ACLs, which a Nix build's seccomp filter refuses: checks.ledger-controller runs it"]
fn on_disk_the_controller_gets_its_access_and_turned_off_keeps_none() {
    let (_root, dir, _) = ledger();
    run(&dir, Some(ME()));
    let key = acl(&dir.join("key"), ACCESS).unwrap();
    assert_eq!(
        entries(&key),
        [
            (USER_OBJ, NO_ID, 6),
            (USER, ME(), 4),
            (GROUP_OBJ, NO_ID, 0),
            (MASK, NO_ID, 4),
            (OTHER, NO_ID, 0)
        ]
    );
    assert_eq!(
        mode(&dir.join("key")),
        0o640,
        "the key's group bits are its mask"
    );
    assert!(entries(&acl(&dir.join("merge-bases"), ACCESS).unwrap()).contains(&(USER, ME(), 6)));
    assert!(entries(&acl(&dir, DEFAULT).unwrap()).contains(&(USER, ME(), 7)));
    assert_eq!(acl(&dir.join("etags"), ACCESS), None);
    // Once set, it's left as it is.
    run(&dir, Some(ME()));
    assert_eq!(acl(&dir.join("key"), ACCESS), Some(key));
    // What either writer makes takes the directory's default entries.
    std::fs::write(dir.join("made"), "").unwrap();
    std::fs::create_dir(dir.join("cache")).unwrap();
    assert!(named(&dir).contains(&ME()));
    run(&dir, None);
    assert_eq!(named(&dir), Vec::<u32>::new());
    for f in ["key", "key.pub", "ops.jsonl", "made", "cache", "etags", "."] {
        let f = dir.join(f);
        assert_eq!(acl(&f, ACCESS), None, "{f:?}");
    }
    assert_eq!(
        mode(&dir.join("key")),
        0o600,
        "the key's group bits are its group's again"
    );
    assert_eq!(mode(&dir.join("ops.jsonl")), 0o644);
}

#[test]
#[ignore = "sets ACLs, which a Nix build's seccomp filter refuses: checks.ledger-controller runs it"]
fn a_link_in_the_ledger_leads_its_access_nowhere() {
    let (_root, dir, outside) = ledger();
    for name in ["merge-bases", "key", "lock"] {
        std::fs::remove_file(dir.join(name)).unwrap();
    }
    symlink(&outside, dir.join("merge-bases")).unwrap();
    symlink(&outside, dir.join("key")).unwrap();
    // A link to the key, by a name the controller may write.
    symlink(dir.join("key"), dir.join("lock")).unwrap();
    run(&dir, Some(ME()));
    assert_eq!(acl(&outside, ACCESS), None);
    assert_eq!(mode(&outside), 0o600);
    for name in ["merge-bases", "key", "lock"] {
        assert!(dir.join(name).is_symlink(), "{name}");
    }
    run(&dir, None);
    assert_eq!(mode(&outside), 0o600);
}

#[test]
#[ignore = "sets ACLs, which a Nix build's seccomp filter refuses: checks.ledger-controller runs it"]
fn a_name_swapped_for_a_link_after_it_was_pinned_changes_only_what_was_pinned() {
    let (_root, dir, outside) = ledger();
    let kept = dir.parent().unwrap().join("old-merge-bases");
    std::fs::hard_link(dir.join("merge-bases"), &kept).unwrap();
    let (pinned, pins) = pin(&dir).unwrap().unwrap();
    // Between pinning and setting, the name is a link to elsewhere.
    let swap = dir.join(".swap");
    symlink(&outside, &swap).unwrap();
    std::fs::rename(&swap, dir.join("merge-bases")).unwrap();
    let mut said = Vec::new();
    assert!(
        !apply(
            &Access {
                owner: ME(),
                controller: Some(ME())
            },
            &pinned,
            &pins,
            &mut |l| said.push(l)
        ),
        "{said:?}"
    );
    assert_eq!(
        acl(&outside, ACCESS),
        None,
        "the link's target is untouched"
    );
    assert_eq!(mode(&outside), 0o600);
    let old = acl(&kept, ACCESS).expect("the file it pinned got the access");
    assert!(entries(&old).contains(&(USER, ME(), 6)));
}
