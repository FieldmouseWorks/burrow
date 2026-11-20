//! The seal, as tests/renovate-sweep.py had it of the Python sealer and
//! seal.py, and pinned to what seal.py made.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use super::{KEY, Verdict, key};

const REPO: &str = "sam/r";
const AB: &str = "abababababababababababababababababababababababababababababababab";

fn sha(n: u64) -> String {
    format!("{n:040x}")
}

/// A sweep's state dir, with `key` as its key when there's one.
fn state(key: Option<&str>) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("renovate-sweep");
    fs::create_dir(&state).unwrap();
    if let Some(k) = key {
        fs::write(state.join(KEY), k).unwrap();
    }
    (dir, state)
}

fn v<'a>(repo: &'a str, n: &'a str, verdict: &'a str, head: &'a str, base: &'a str) -> Verdict<'a> {
    Verdict {
        repo,
        n,
        verdict,
        head,
        base,
    }
}

/// The MAC in a marker.
fn mac_of(marker: &str) -> &str {
    marker.split(' ').rev().nth(1).unwrap()
}

fn sealed(state: &Path, verdict: &Verdict, mac: &str) -> bool {
    verdict.sealed(state, mac).unwrap()
}

/// What the Python sealer printed and seal.py's major() gave under these keys,
/// before the sweep sealed in Rust: the same marker, byte for byte, and the
/// same MAC, each field in its place and the repo in lower case. A mixed-case
/// repo seals as its lower case does (seal.py's raw mac() of `Sam/R` is
/// 214afc04449a985e3e282a5105998a49, which this would give were the case
/// kept).
#[test]
fn it_seals_as_seal_py_did() {
    let (_d, st) = state(Some(AB));
    let one = sha(1);
    let three = sha(3);
    assert_eq!(
        v(REPO, "1", "ok", &one, "main").marker(&st).unwrap(),
        "<!-- major-verdict: ok 0000000000000000000000000000000000000001 on main 61fd5bc62f736dc75e10d05aec24fce6 -->"
    );
    assert_eq!(
        v("Sam/R", "3", "held", &three, "release").marker(&st).unwrap(),
        "<!-- major-verdict: held 0000000000000000000000000000000000000003 on release aa684489b31dad88304889f62a4653c6 -->"
    );
    let k2 = "0123456789abcdef".repeat(4);
    let (_d, st2) = state(Some(&k2));
    let head = "deadbeef".repeat(5);
    for (repo, n, verdict, base, want) in [
        ("FieldmouseWorks/Burrow", "1234", "ok", "main", "9c3a24e563c5e30267a9e6567f989355"),
        ("fieldmouseworks/burrow", "1234", "held", "release/1.x", "76db532a9ac56525af1f4f73709f5747"),
    ] {
        let verdict = v(repo, n, verdict, &head, base);
        assert_eq!(mac_of(&verdict.marker(&st2).unwrap()), want, "{repo} {n}");
        assert!(sealed(&st2, &verdict, want));
    }
}

/// A check takes the seal the sweep made, the repo in any case (a clone's
/// remote needn't spell it as GitHub does), and nothing else: another
/// verdict, head, base, PR or repo, another key's, or one with no key.
#[test]
fn a_check_takes_its_own_seal_and_nothing_else() {
    let (_d, st) = state(Some(AB));
    let one = sha(1);
    let two = sha(2);
    let mac = mac_of(&v(REPO, "1", "ok", &one, "main").marker(&st).unwrap()).to_owned();
    assert!(sealed(&st, &v(REPO, "1", "ok", &one, "main"), &mac));
    assert!(sealed(&st, &v("Sam/R", "1", "ok", &one, "main"), &mac));
    for other in [
        v(REPO, "1", "held", &one, "main"),
        v(REPO, "1", "ok", &two, "main"),
        v(REPO, "1", "ok", &one, "release"),
        v(REPO, "3", "ok", &one, "main"),
        v("sam/q", "1", "ok", &one, "main"),
        // A number no marker names, though it reads as the same.
        v(REPO, "01", "ok", &one, "main"),
        v(REPO, "+1", "ok", &one, "main"),
    ] {
        assert!(!sealed(&st, &other, &mac), "{} {}", other.repo, other.n);
    }
    assert!(!sealed(&st, &v(REPO, "1", "ok", &one, "main"), &mac[..31]));
    let (_d, other) = state(Some(&"cd".repeat(32)));
    assert!(!sealed(&other, &v(REPO, "1", "ok", &one, "main"), &mac));
}

/// A check with no key fails and makes none; one with a damaged key fails
/// and leaves it be.
#[test]
fn a_check_never_makes_a_key() {
    let one = sha(1);
    let (_d, st) = state(None);
    assert!(!sealed(&st, &v(REPO, "1", "ok", &one, "main"), &"0".repeat(32)));
    assert!(!st.join(KEY).exists());
    let (_d, st) = state(Some("abc"));
    assert!(!sealed(&st, &v(REPO, "1", "ok", &one, "main"), &"0".repeat(32)));
    assert_eq!(fs::read_to_string(st.join(KEY)).unwrap(), "abc");
    // And one it can't read is a failure, not a pass.
    let (_d, st) = state(None);
    fs::create_dir(st.join(KEY)).unwrap();
    assert!(v(REPO, "1", "ok", &one, "main").sealed(&st, &"0".repeat(32)).is_err());
}

/// A key it makes when there's none is one a check takes after: the
/// owner's alone, whole.
#[test]
fn a_seal_makes_a_key_when_there_is_none() {
    let one = sha(1);
    let (_d, st) = state(None);
    let marker = v(REPO, "1", "held", &one, "main").marker(&st).unwrap();
    assert!(sealed(&st, &v(REPO, "1", "held", &one, "main"), mac_of(&marker)));
    let path = st.join(KEY);
    let made = fs::read_to_string(&path).unwrap();
    assert!(made.len() == 64 && made.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
    assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o077, 0);
    // No temporary left beside it, and the next seal keeps it.
    assert_eq!(fs::read_dir(&st).unwrap().count(), 1);
    v(REPO, "2", "ok", &sha(2), "main").marker(&st).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), made);
    // One made meanwhile wins over its own: linked only if absent.
    assert_eq!(key(&path, true).unwrap().unwrap(), super::parse(made.as_bytes()).unwrap());
}

/// A damaged key (empty, short, upper case, not text) is replaced whole by
/// the next seal, and verdicts sealed with it stop counting.
#[test]
fn a_damaged_key_is_replaced() {
    let one = sha(1);
    for damaged in [&b""[..], b"abc", AB[..63].as_bytes(), AB.to_uppercase().as_bytes(), b"\xff\xfe"] {
        let (_d, st) = state(None);
        let path = st.join(KEY);
        fs::write(&path, damaged).unwrap();
        let marker = v(REPO, "1", "ok", &one, "main").marker(&st).unwrap();
        let made = fs::read(&path).unwrap();
        assert!(made.len() == 64 && made != damaged, "{damaged:?}");
        assert!(sealed(&st, &v(REPO, "1", "ok", &one, "main"), mac_of(&marker)));
        assert_eq!(fs::read_dir(&st).unwrap().count(), 1);
    }
    // A key with whitespace around it is still the key.
    let (_d, st) = state(Some(&format!("{AB}\n")));
    let marker = v(REPO, "1", "ok", &one, "main").marker(&st).unwrap();
    assert_eq!(mac_of(&marker), "61fd5bc62f736dc75e10d05aec24fce6");
    assert_eq!(fs::read_to_string(st.join(KEY)).unwrap(), format!("{AB}\n"));
}

/// It seals only what it can read back: a repo, PR, verdict, head and base
/// each as a marker names them. A refused one makes no key.
#[test]
fn it_seals_only_what_a_marker_can_name() {
    let one = sha(1);
    for bad in [
        v("sam", "1", "ok", &one, "main"),
        v("sam/r x", "1", "ok", &one, "main"),
        v("sam/r/x", "1", "ok", &one, "main"),
        v(REPO, "x", "ok", &one, "main"),
        v(REPO, "0", "ok", &one, "main"),
        v(REPO, "01", "ok", &one, "main"),
        v(REPO, "+1", "ok", &one, "main"),
        v(REPO, "1", "maybe", &one, "main"),
        v(REPO, "1", "ok", "abc", "main"),
        v(REPO, "1", "ok", &one.to_uppercase(), "main"),
        v(REPO, "1", "ok", &one, "a>b"),
        v(REPO, "1", "ok", &one, "a b"),
        v(REPO, "1", "ok", &one, ""),
    ] {
        let (_d, st) = state(None);
        assert!(bad.marker(&st).is_err(), "{} {} {} {} {}", bad.repo, bad.n, bad.verdict, bad.head, bad.base);
        assert!(!st.join(KEY).exists());
    }
}
