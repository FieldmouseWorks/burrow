use super::*;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;

const ME: fn() -> u32 = || rustix::process::getuid().as_raw();
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
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    std::fs::create_dir(dir.join("etags")).unwrap();
    let outside = root.path().join("secret");
    std::fs::write(&outside, "secret").unwrap();
    std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o600)).unwrap();
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

#[test]
fn the_controller_signs_and_no_one_else_reads_the_key() {
    let (_root, dir, _) = ledger();
    run(&dir, Some(FLOW));
    let rwx = 7;
    let key = acl(&dir.join("key"), ACCESS).unwrap();
    assert_eq!(
        entries(&key),
        [
            (USER_OBJ, NO_ID, 6),
            (USER, FLOW, 4),
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
    for w in WRITABLE {
        let a = acl(&dir.join(w), ACCESS).unwrap();
        assert_eq!(
            entries(&a),
            [
                (USER_OBJ, NO_ID, 6),
                (USER, FLOW, 6),
                (GROUP_OBJ, NO_ID, 4),
                (MASK, NO_ID, 6),
                (OTHER, NO_ID, 4)
            ],
            "{w}"
        );
    }
    let d = acl(&dir, ACCESS).unwrap();
    assert!(entries(&d).contains(&(USER, FLOW, rwx)));
    let default = acl(&dir, DEFAULT).unwrap();
    assert!(
        entries(&default).contains(&(USER, FLOW, rwx))
            && entries(&default).contains(&(USER, ME(), rwx))
    );
    assert_eq!(acl(&dir.join("key.pub"), ACCESS), None);
    assert_eq!(acl(&dir.join("etags"), ACCESS), None);
    // Once set, it's left as it is.
    run(&dir, Some(FLOW));
    assert_eq!(acl(&dir.join("key"), ACCESS), Some(key));
}

#[test]
fn a_controller_changed_or_turned_off_keeps_nothing() {
    let (_root, dir, _) = ledger();
    run(&dir, Some(FLOW));
    // What either writer makes takes the directory's default entries.
    std::fs::write(dir.join("made"), "").unwrap();
    std::fs::create_dir(dir.join("cache")).unwrap();
    assert!(named(&dir).contains(&FLOW));
    run(&dir, Some(MOVED));
    assert!(!named(&dir).contains(&FLOW), "{:?}", named(&dir));
    assert!(named(&dir).contains(&MOVED));
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
fn a_link_in_the_ledger_leads_its_access_nowhere() {
    let (_root, dir, outside) = ledger();
    for name in ["merge-bases", "key", "lock"] {
        std::fs::remove_file(dir.join(name)).unwrap();
    }
    symlink(&outside, dir.join("merge-bases")).unwrap();
    symlink(&outside, dir.join("key")).unwrap();
    // A link to the key, by a name the controller may write.
    symlink(dir.join("key"), dir.join("lock")).unwrap();
    run(&dir, Some(FLOW));
    assert_eq!(acl(&outside, ACCESS), None);
    assert_eq!(mode(&outside), 0o600);
    for name in ["merge-bases", "key", "lock"] {
        assert!(dir.join(name).is_symlink(), "{name}");
    }
    run(&dir, None);
    assert_eq!(mode(&outside), 0o600);
}

#[test]
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
                controller: Some(FLOW)
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
    assert!(entries(&old).contains(&(USER, FLOW, 6)));
}
