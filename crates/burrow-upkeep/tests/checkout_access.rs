//! Publishing a clone over an empty folder keeps that folder's access
//! controls: owner, group, mode bits and ACLs, default ACLs included, so what
//! the clone writes inherits them as it would cloning into the folder itself.
//! Its own test binary: the checkout makes the process a subreaper, and reaps
//! every child, so its cases run one after another.
use burrow_upkeep::checkout::clone_with;
use rustix::fs::{XattrFlags, fgetxattr, fremovexattr, fsetxattr};
use rustix::io::Errno;
use rustix::time::{ClockId, clock_gettime};
use std::fs;
use std::os::unix::fs::{MetadataExt, chown};
use std::path::Path;

const ACCESS: &str = "system.posix_acl_access";
const DEFAULT: &str = "system.posix_acl_default";

fn deadline() -> f64 {
    let t = clock_gettime(ClockId::Monotonic);
    t.tv_sec as f64 + t.tv_nsec as f64 / 1e9 + 30.0
}

/// An ACL as the kernel stores it: version 2, then (tag, perm, id) entries
/// sorted by tag. Tags: 1 owner, 2 named user, 4 group, 0x10 mask, 0x20 other.
fn acl(entries: &[(u16, u16, u32)]) -> Vec<u8> {
    let mut out = 2u32.to_le_bytes().to_vec();
    for (tag, perm, id) in entries {
        out.extend(tag.to_le_bytes());
        out.extend(perm.to_le_bytes());
        out.extend(id.to_le_bytes());
    }
    out
}

const ANY: u32 = u32::MAX;

fn get(path: &Path, name: &str) -> Option<Vec<u8>> {
    let f = fs::File::open(path).unwrap();
    let mut buf = vec![0u8; 4096];
    match fgetxattr(&f, name, &mut buf[..]) {
        Ok(n) => Some(buf[..n].to_vec()),
        // No ACL, or (in Nix's sandbox) no xattrs to read at all.
        Err(Errno::NODATA | Errno::NOTSUP) => None,
        Err(e) => panic!("{}: {name}: {e}", path.display()),
    }
}

fn set(path: &Path, name: &str, value: &[u8]) {
    let f = fs::File::open(path).unwrap();
    fsetxattr(&f, name, value, XattrFlags::empty()).unwrap_or_else(|e| {
        panic!(
            "{}: {name}: {e} (this test needs POSIX ACLs)",
            path.display()
        )
    });
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().mode() & 0o7777
}

/// What the published checkout's root must keep of its folder.
fn controls(path: &Path) -> (u32, u32, u32, Option<Vec<u8>>, Option<Vec<u8>>) {
    let m = fs::metadata(path).unwrap();
    (
        m.uid(),
        m.gid(),
        m.mode() & 0o7777,
        get(path, ACCESS),
        get(path, DEFAULT),
    )
}

#[test]
fn publication_keeps_the_folders_access_controls() {
    let tmp = tempfile::tempdir().unwrap();
    let gh = tmp.path().join("gh");
    // The clone makes a folder and a file, inheriting from its root.
    burrow_test::executable(
        &gh,
        "#!/bin/sh\nmkdir -p \"$4/sub\" && echo x > \"$4/sub/file\"\n",
    );
    let gh = gh.display().to_string();
    let projects = tmp.path().join("projects");
    fs::create_dir(&projects).unwrap();
    let me = rustix::process::geteuid().as_raw();
    let mine = rustix::process::getegid().as_raw();

    // Restrictive: a private folder stays private, not the umask's 0755.
    let dest = projects.join("private");
    fs::create_dir(&dest).unwrap();
    burrow_test::permissions(&dest, 0o700);
    let had = controls(&dest);
    clone_with(&gh, "o/private", &dest, deadline()).unwrap();
    assert_eq!(controls(&dest), had, "a private folder's controls");
    assert!(dest.join("sub/file").exists());

    // Shared: setgid and group write, on another of our groups if we have one,
    // so the clone's folders take its group as they would in place.
    let dest = projects.join("shared");
    fs::create_dir(&dest).unwrap();
    let other = rustix::process::getgroups()
        .unwrap()
        .into_iter()
        .map(|g| g.as_raw())
        .find(|g| *g != mine);
    if let Some(g) = other {
        chown(&dest, None, Some(g)).unwrap();
    }
    // Nix's build sandbox refuses setgid with EPERM: there the sticky bit
    // stands in for it as the special bit kept.
    let setgid = burrow_test::try_permissions(&dest, 0o2775).is_ok();
    if !setgid {
        eprintln!("setgid can't be set here: the sticky bit stands in");
        burrow_test::permissions(&dest, 0o1775);
    }
    let had = controls(&dest);
    clone_with(&gh, "o/shared", &dest, deadline()).unwrap();
    assert_eq!(controls(&dest), had, "a shared folder's controls");
    if setgid {
        let sub = fs::metadata(dest.join("sub")).unwrap();
        assert_eq!(sub.gid(), had.1, "setgid's group");
        assert_ne!(sub.mode() & 0o2000, 0, "setgid passed down");
    }

    // Absent: the checkout is made as a mkdir in its parent would make it.
    let dest = projects.join("new");
    let sibling = projects.join("sibling");
    fs::create_dir(&sibling).unwrap();
    clone_with(&gh, "o/new", &dest, deadline()).unwrap();
    assert_eq!(controls(&dest), controls(&sibling), "an absent folder");

    // Nix's build sandbox refuses every setxattr with ENOTSUP, so the ACL
    // cases run wherever ACLs can be set (cargo test on the machine) and say
    // so where they can't.
    let probe = projects.join("probe");
    fs::create_dir(&probe).unwrap();
    let f = fs::File::open(&probe).unwrap();
    let base = acl(&[(1, 7, ANY), (4, 5, ANY), (0x20, 0, ANY)]);
    match fsetxattr(&f, DEFAULT, &base, XattrFlags::empty()) {
        Ok(()) => acl_cases(tmp.path(), &projects, &gh, me),
        Err(Errno::NOTSUP) => eprintln!("POSIX ACLs can't be set here: their cases didn't run"),
        Err(e) => panic!("probing ACLs: {e}"),
    }

    // A folder filled during the clone is neither replaced nor changed.
    let dest = projects.join("filled");
    fs::create_dir(&dest).unwrap();
    burrow_test::permissions(&dest, 0o750);
    let racer = tmp.path().join("racer");
    burrow_test::executable(
        &racer,
        format!(
            "#!/bin/sh\nmkdir -p \"$4/sub\" && echo keep > {}/keep\n",
            dest.display()
        ),
    );
    let had = controls(&dest);
    assert!(clone_with(&racer.display().to_string(), "o/filled", &dest, deadline()).is_err());
    assert_eq!(controls(&dest), had, "a filled folder's controls");
    assert_eq!(fs::read_to_string(dest.join("keep")).unwrap(), "keep\n");
    assert!(!dest.join("sub").exists(), "published over local data");
    assert_eq!(had.0, me);
}

fn acl_cases(tmp: &Path, projects: &Path, gh: &str, me: u32) {
    // ACLs: a named user's access, and a default ACL the clone inherits.
    let dest = projects.join("acl");
    fs::create_dir(&dest).unwrap();
    let access = acl(&[
        (1, 7, ANY),
        (2, 5, me),
        (4, 0, ANY),
        (0x10, 5, ANY),
        (0x20, 0, ANY),
    ]);
    let default = acl(&[(1, 7, ANY), (4, 5, ANY), (0x20, 0, ANY)]);
    set(&dest, ACCESS, &access);
    set(&dest, DEFAULT, &default);
    let had = controls(&dest);
    assert_eq!(
        (had.3.as_deref(), had.4.as_deref()),
        (Some(&access[..]), Some(&default[..]))
    );
    clone_with(gh, "o/acl", &dest, deadline()).unwrap();
    assert_eq!(controls(&dest), had, "a folder's ACLs");
    let sub = dest.join("sub");
    assert_eq!(
        mode(&sub),
        0o750,
        "the clone inherited the folder's default ACL"
    );
    assert_eq!(get(&sub, DEFAULT).as_deref(), Some(&default[..]));

    // The staging area's inheritance doesn't stand in for the folder's: a
    // parent's default ACL the folder has dropped stays dropped.
    let open = projects.join("open");
    fs::create_dir(&open).unwrap();
    set(
        &open,
        DEFAULT,
        &acl(&[(1, 7, ANY), (4, 7, ANY), (0x20, 7, ANY)]),
    );
    let dest = open.join("app");
    fs::create_dir(&dest).unwrap();
    let f = fs::File::open(&dest).unwrap();
    for name in [ACCESS, DEFAULT] {
        match fremovexattr(&f, name) {
            Ok(()) | Err(Errno::NODATA) => {}
            Err(e) => panic!("{name}: {e}"),
        }
    }
    burrow_test::permissions(&dest, 0o700);
    let had = controls(&dest);
    assert_eq!((had.3.clone(), had.4.clone()), (None, None));
    clone_with(gh, "o/app", &dest, deadline()).unwrap();
    assert_eq!(controls(&dest), had, "no ACL came from the parent");
    let sub = dest.join("sub");
    assert_eq!(get(&sub, DEFAULT), None);
    let plain = tmp.join("plain");
    fs::create_dir(&plain).unwrap();
    assert_eq!(mode(&sub), mode(&plain), "the umask, not the parent's 0777");

    // Absent: the checkout takes its parent's default ACL, as a mkdir would.
    let dest = open.join("new");
    let sibling = open.join("sibling");
    fs::create_dir(&sibling).unwrap();
    clone_with(gh, "o/new", &dest, deadline()).unwrap();
    assert_eq!(controls(&dest), controls(&sibling), "an absent folder");
}
