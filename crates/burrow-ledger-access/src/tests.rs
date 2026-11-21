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
fn an_acl_stripped_leaves_no_class_more_than_it_had() {
    // etags as Conary's 0700 mkdir leaves it under the default entries.
    let etags = Base {
        user: 7,
        group: 5,
        other: 0,
    }
    .with(&[(FLOW, 7)], 0);
    assert_eq!(stripped(0o40700, &etags), 0o700);
    let key = Base {
        user: 6,
        group: 0,
        other: 0,
    }
    .with(&[(FLOW, 4)], 4);
    assert_eq!(stripped(0o100640, &key), 0o600);
    let log = Base {
        user: 6,
        group: 4,
        other: 4,
    }
    .with(&[(FLOW, 6)], 6);
    assert_eq!(stripped(0o102664, &log), 0o2644, "set-id bits stay");
    // An ACL with no mask: the owning group's entry is what it had.
    let plain = Acl(vec![
        Entry {
            tag: USER_OBJ,
            id: NO_ID,
            perm: 6,
        },
        Entry {
            tag: GROUP_OBJ,
            id: NO_ID,
            perm: 4,
        },
        Entry {
            tag: OTHER,
            id: NO_ID,
            perm: 0,
        },
    ]);
    assert_eq!(stripped(0o100640, &plain), 0o640);
}

#[test]
fn a_chmod_after_the_acl_was_read_stays() {
    // An answer a writer made under etags' default entries, as read before
    // the writer's chmod 600: owning group r-x through the mask rw-.
    let read = Base {
        user: 6,
        group: 5,
        other: 4,
    }
    .with(&[(FLOW, 7)], 6);
    // The chmod rewrote the mask, so with the ACL off the mode is 0600.
    assert_eq!(stripped(0o100600, &read), 0o600, "the chmod isn't undone");
    // With no chmod between, the group keeps what it had in effect.
    assert_eq!(stripped(0o100664, &read), 0o644);
}

#[test]
fn what_a_directory_holds_gets_no_class_it_can_not_reach() {
    assert_eq!(reach(0o40700), 0o700);
    assert_eq!(reach(0o40750), 0o770);
    assert_eq!(reach(0o40755), 0o777);
    assert_eq!(reach(0o40644), 0);
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
    // etags made as Conary makes it, under the directory's default entries,
    // keeps the 0700 it had in effect once the next run strips them.
    std::fs::remove_dir(dir.join("etags")).unwrap();
    std::os::unix::fs::DirBuilderExt::mode(&mut std::fs::DirBuilder::new(), 0o700)
        .create(dir.join("etags"))
        .unwrap();
    // And an answer in it, 0600 as Conary's tempfile is, which takes the
    // default entries etags took.
    let answer = dir.join("etags/h1");
    std::fs::write(&answer, "etag").unwrap();
    burrow_test::permissions(&answer, 0o600);
    assert!(acl(&dir.join("etags"), ACCESS).is_some());
    assert!(acl(&answer, ACCESS).is_some());
    run(&dir, Some(ME()));
    assert_eq!(acl(&dir.join("etags"), ACCESS), None);
    assert_eq!(acl(&dir.join("etags"), DEFAULT), None);
    assert_eq!(mode(&dir.join("etags")), 0o700, "etags isn't widened");
    assert_eq!(acl(&answer, ACCESS), None, "what etags holds names no one");
    assert_eq!(mode(&answer), 0o600);
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

#[test]
#[ignore = "sets ACLs, which a Nix build's seccomp filter refuses: checks.ledger-controller runs it"]
fn an_answer_chmodded_while_its_acl_comes_off_keeps_the_chmod() {
    let (_root, dir, _) = ledger();
    run(&dir, Some(ME()));
    std::fs::remove_dir(dir.join("etags")).unwrap();
    std::os::unix::fs::DirBuilderExt::mode(&mut std::fs::DirBuilder::new(), 0o700)
        .create(dir.join("etags"))
        .unwrap();
    // Made as a writer that chmods after it writes makes it: wide, under
    // the default entries etags took.
    let answer = dir.join("etags/h1");
    std::fs::write(&answer, "etag").unwrap();
    let (_, pins) = pin(&dir.join("etags")).unwrap().unwrap();
    let [h1] = &pins[..] else { panic!("{pins:?}") };
    let read = read(&h1.proc(), ACCESS)
        .unwrap()
        .expect("it took the default entries");
    assert_ne!(mode(&answer) & 0o077, 0, "made wider than 0600");
    // The writer's chmod, between the read and the ACL coming off.
    burrow_test::permissions(&answer, 0o600);
    unacl(h1, Some(&read), 0o777).unwrap();
    assert_eq!(acl(&answer, ACCESS), None);
    assert_eq!(mode(&answer), 0o600, "the writer's chmod stays");
}

#[test]
#[ignore = "sets ACLs, which a Nix build's seccomp filter refuses: checks.ledger-controller runs it"]
fn what_etags_holds_is_checked_once_etags_itself_names_no_one() {
    let (_root, dir, _) = ledger();
    run(&dir, Some(ME()));
    std::fs::remove_dir(dir.join("etags")).unwrap();
    std::os::unix::fs::DirBuilderExt::mode(&mut std::fs::DirBuilder::new(), 0o700)
        .create(dir.join("etags"))
        .unwrap();
    let late = dir.join("etags/late");
    std::fs::write(&late, "etag").unwrap();
    assert!(acl(&late, ACCESS).is_some());
    // etags itself named no one by then: a run already took its entries.
    let etags = dir.join("etags");
    let etags = etags.to_str().unwrap();
    fs::lremovexattr(etags, ACCESS).unwrap();
    fs::lremovexattr(etags, DEFAULT).unwrap();
    burrow_test::permissions(dir.join("etags"), 0o700);
    // And an answer a run left wider than etags lets anyone but its owner reach.
    let wide = dir.join("etags/wide");
    std::fs::write(&wide, "etag").unwrap();
    fs::lremovexattr(wide.to_str().unwrap(), ACCESS).unwrap();
    burrow_test::permissions(&wide, 0o644);
    run(&dir, Some(ME()));
    assert_eq!(acl(&late, ACCESS), None, "what etags holds names no one");
    assert_eq!(mode(&late) & 0o077, 0);
    assert_eq!(mode(&wide), 0o600, "no wider than etags lets it be reached");
    assert_eq!(mode(&dir.join("etags")), 0o700);
}
