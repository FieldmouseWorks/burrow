//! The login's copies with nothing watching them: `sync` runs only where a
//! test calls it, so none of this leans on when a watcher gets to a write.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use serde_json::json;
use tempfile::TempDir;

use super::{State, open_dir};

fn auth(k: &str) -> String {
    json!({"tokens": {"refresh_token": k}}).to_string()
}

const A: &str = "keyAAAAAAAAAAAAAAAAAAAAA";
const B: &str = "keyBBBBBBBBBBBBBBBBBBBBB";
const C: &str = "keyCCCCCCCCCCCCCCCCCCCCC";
const D: &str = "keyDDDDDDDDDDDDDDDDDDDDD";

struct Box {
    t: TempDir,
    state: State,
}

impl Box {
    fn new(login: &str) -> Box {
        let t = TempDir::new().unwrap();
        fs::create_dir(t.path().join(".codex")).unwrap();
        let mut b = Box {
            state: State {
                dir: t.path().join(".codex"),
                ..State::default()
            },
            t,
        };
        fs::write(b.login(), auth(login)).unwrap();
        b.state.sync().unwrap();
        b
    }

    fn login(&self) -> PathBuf {
        self.t.path().join(".codex/auth.json")
    }

    /// A look's home, lent its copy.
    fn look(&mut self, name: &str) -> (u64, PathBuf) {
        let home = self.t.path().join(name);
        fs::create_dir(&home).unwrap();
        let id = self.state.lend(open_dir(&home).unwrap()).unwrap();
        (id, home.join("auth.json"))
    }

    fn seen(&self, k: &str) -> bool {
        self.state.seen.contains(k)
    }
}

/// A login saved the way a save leaves one whole.
fn saved_over(at: &Path, what: &str) {
    let tmp = at.with_extension("new");
    fs::write(&tmp, what).unwrap();
    fs::rename(tmp, at).unwrap();
}

/// A copy is a file of the look's own, for its owner: not a link to the
/// login, so writing it can't touch what the look beside it wrote.
#[test]
fn each_look_has_a_file_of_its_own() {
    let mut b = Box::new(A);
    let ((_, one), (_, two)) = (b.look("one"), b.look("two"));
    for copy in [&one, &two] {
        let m = fs::symlink_metadata(copy).unwrap();
        assert!(m.is_file(), "{copy:?}");
        assert_eq!(m.permissions().mode() & 0o777, 0o600);
        assert_eq!(fs::read_to_string(copy).unwrap(), auth(A));
    }
    fs::write(&one, auth(B)).unwrap();
    assert_eq!(fs::read_to_string(&two).unwrap(), auth(A));
    assert_eq!(fs::read_to_string(b.login()).unwrap(), auth(A));
}

/// Two looks write at once, nothing reading between: neither loses a key
/// to the other, in place or saved over.
#[test]
fn looks_writing_at_once_lose_no_key() {
    for write in [
        (|at: &Path, what: &str| fs::write(at, what).unwrap()) as fn(&Path, &str),
        saved_over,
    ] {
        let mut b = Box::new(A);
        let ((_, one), (_, two)) = (b.look("one"), b.look("two"));
        write(&one, &auth(B));
        write(&two, &auth(C));
        b.state.sync().unwrap();
        assert!(b.seen(A) && b.seen(B) && b.seen(C), "{:?}", b.state.seen);
    }
}

/// What one look refreshed reaches the login and the look beside it, its
/// keys kept first, and later writes over it don't lose them.
#[test]
fn a_refresh_goes_to_everyone_once_its_kept() {
    let mut b = Box::new(A);
    let ((_, one), (_, two)) = (b.look("one"), b.look("two"));
    fs::write(&one, auth(B)).unwrap();
    b.state.sync().unwrap();
    assert!(b.seen(B));
    assert_eq!(fs::read_to_string(&two).unwrap(), auth(B));
    assert_eq!(fs::read_to_string(b.login()).unwrap(), auth(B));
    fs::write(&two, auth(C)).unwrap();
    b.state.sync().unwrap();
    assert_eq!(fs::read_to_string(&one).unwrap(), auth(C));
    assert_eq!(fs::read_to_string(b.login()).unwrap(), auth(C));
    assert!(b.seen(A) && b.seen(B) && b.seen(C));
}

/// A login replaced on the machine (its path saved over) while a look runs
/// neither loses what the look writes after nor keeps the look from it.
#[test]
fn a_login_replaced_under_a_look_loses_nothing() {
    let mut b = Box::new(A);
    let (_, one) = b.look("one");
    saved_over(&b.login(), &auth(B));
    fs::write(&one, auth(C)).unwrap();
    b.state.sync().unwrap();
    assert!(b.seen(A) && b.seen(B) && b.seen(C));
    assert_eq!(fs::read_to_string(b.login()).unwrap(), auth(C));

    saved_over(&b.login(), &auth(D));
    b.state.sync().unwrap();
    assert_eq!(fs::read_to_string(&one).unwrap(), auth(D));
}

/// A write a look began just before its copy was replaced lands in the old
/// file: its keys are kept, and it's a refresh like any other.
#[test]
fn a_write_into_a_replaced_copy_still_counts() {
    let mut b = Box::new(A);
    let ((_, one), (_, two)) = (b.look("one"), b.look("two"));
    let mut under_way = OpenOptions::new().write(true).open(&two).unwrap();
    fs::write(&one, auth(B)).unwrap();
    b.state.sync().unwrap();
    assert_eq!(fs::read_to_string(&two).unwrap(), auth(B));
    under_way.set_len(0).unwrap();
    under_way.write_all(auth(C).as_bytes()).unwrap();
    drop(under_way);
    b.state.sync().unwrap();
    assert!(b.seen(C));
    assert_eq!(fs::read_to_string(b.login()).unwrap(), auth(C));
    assert_eq!(fs::read_to_string(&two).unwrap(), auth(C));
}

/// Half a write isn't a login: nothing changes until the rest arrives.
#[test]
fn half_a_write_changes_nothing() {
    let mut b = Box::new(A);
    let (_, one) = b.look("one");
    fs::write(&one, &auth(B)[..9]).unwrap();
    b.state.sync().unwrap();
    assert_eq!(fs::read_to_string(b.login()).unwrap(), auth(A));
    fs::write(&one, auth(B)).unwrap();
    b.state.sync().unwrap();
    assert_eq!(fs::read_to_string(b.login()).unwrap(), auth(B));
}

/// Giving it back reads it one last time, then takes it away; a link from
/// before and a copy a killed service left are lent over, the left one's
/// keys kept.
#[test]
fn a_copy_is_read_last_when_given_back() {
    let mut b = Box::new(A);
    let (id, one) = b.look("one");
    fs::write(&one, auth(B)).unwrap();
    b.state.give_back(id).unwrap();
    assert!(b.seen(B));
    assert!(fs::symlink_metadata(&one).is_err());
    assert_eq!(fs::read_to_string(b.login()).unwrap(), auth(B));

    let home = b.t.path().join("linked");
    fs::create_dir(&home).unwrap();
    symlink(b.login(), home.join("auth.json")).unwrap();
    b.state.lend(open_dir(&home).unwrap()).unwrap();
    assert!(
        fs::symlink_metadata(home.join("auth.json"))
            .unwrap()
            .is_file()
    );

    let home = b.t.path().join("left");
    fs::create_dir(&home).unwrap();
    fs::write(home.join("auth.json"), auth(D)).unwrap();
    b.state.lend(open_dir(&home).unwrap()).unwrap();
    assert!(b.seen(D));
    assert_eq!(fs::read_to_string(home.join("auth.json")).unwrap(), auth(B));
}

/// No login on the machine: nothing lent, and a link left there goes.
#[test]
fn no_login_lends_nothing() {
    let mut b = Box::new(A);
    fs::remove_file(b.login()).unwrap();
    b.state.sync().unwrap();
    let home = b.t.path().join("one");
    fs::create_dir(&home).unwrap();
    symlink(b.login(), home.join("auth.json")).unwrap();
    b.state.lend(open_dir(&home).unwrap()).unwrap();
    assert!(fs::symlink_metadata(home.join("auth.json")).is_err());
}
