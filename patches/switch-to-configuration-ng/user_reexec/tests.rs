//! user_reexec.rs's own: when a user manager is re-executed.

use super::*;

const NEW: &str = "/nix/store/new-systemd/lib/systemd/systemd";
const OLD: &str = "/nix/store/old-systemd/lib/systemd/systemd";

#[test]
fn a_manager_on_the_new_systemd_is_left_running() {
    assert_eq!(value(Some(Path::new(NEW)), Path::new(NEW)), "0");
    assert!(!wanted(Some("0")));
}

#[test]
fn a_manager_on_another_systemd_is_re_executed() {
    assert_eq!(value(Some(Path::new(OLD)), Path::new(NEW)), "1");
    assert!(wanted(Some("1")));
}

#[test]
fn a_binary_it_cant_read_re_executes_as_before() {
    // A main process gone, a unit not loaded, a binary since deleted.
    assert_eq!(value(None, Path::new(NEW)), "1");
    assert_eq!(value(Some(Path::new(&format!("{NEW} (deleted)"))), Path::new(NEW)), "1");
    // An activation the parent didn't tell, or told something else.
    assert!(wanted(None));
    assert!(wanted(Some("")));
    assert!(wanted(Some("yes")));
}
