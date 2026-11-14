//! user_reexec.rs's own: when a user manager is re-executed.

use super::*;

const NEW: &str = "/nix/store/new-systemd/lib/systemd/systemd";
const OLD: &str = "/nix/store/old-systemd/lib/systemd/systemd";

/// A fresh directory of this test's own.
fn dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("user-reexec-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(path: PathBuf, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[test]
fn a_manager_on_the_new_systemd_and_config_is_left_running() {
    assert_eq!(value(Some(Path::new(NEW)), Path::new(NEW), false), "0");
    assert!(!wanted(Some("0"), false));
}

#[test]
fn a_manager_on_another_systemd_is_re_executed() {
    assert_eq!(value(Some(Path::new(OLD)), Path::new(NEW), false), "1");
    assert!(wanted(Some("1"), false));
}

#[test]
fn a_changed_user_conf_is_re_executed_for() {
    // TimerSlackNSec, SystemCallArchitectures: applied only by a starting
    // manager, so a reload alone would leave them unapplied.
    assert_eq!(value(Some(Path::new(NEW)), Path::new(NEW), true), "1");

    let root = dir("system");
    let (old, new) = (root.join("old"), root.join("new"));
    write(old.join("user.conf"), "[Manager]\n");
    write(new.join("user.conf"), "[Manager]\n");
    assert!(!config_changed(&old, &new));
    write(new.join("user.conf"), "[Manager]\nTimerSlackNSec=1ms\n");
    assert!(config_changed(&old, &new));
    write(new.join("user.conf"), "[Manager]\n");
    // A drop-in added, then changed, then the same on both sides.
    write(new.join("user.conf.d/arch.conf"), "[Manager]\nSystemCallArchitectures=native\n");
    assert!(config_changed(&old, &new));
    write(old.join("user.conf.d/arch.conf"), "[Manager]\n");
    assert!(config_changed(&old, &new));
    write(old.join("user.conf.d/arch.conf"), "[Manager]\nSystemCallArchitectures=native\n");
    assert!(!config_changed(&old, &new));
    // Neither system has one: nothing changed.
    assert!(!config_changed(&root.join("none"), &root.join("none-either")));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_user_with_a_config_of_their_own_is_re_executed_as_before() {
    let home = dir("home");
    let systemd = home.join("systemd");
    std::fs::create_dir_all(systemd.join("user")).unwrap();
    assert!(!own_config(Some(&systemd)));
    assert!(!wanted(Some("0"), own_config(Some(&systemd))));
    write(systemd.join("user.conf.d/slack.conf"), "[Manager]\nTimerSlackNSec=1ms\n");
    assert!(own_config(Some(&systemd)));
    std::fs::remove_dir_all(systemd.join("user.conf.d")).unwrap();
    write(systemd.join("user.conf"), "[Manager]\n");
    assert!(own_config(Some(&systemd)));
    assert!(wanted(Some("0"), own_config(Some(&systemd))));
    // Where it is can't be told.
    assert!(own_config(None));
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn what_it_cant_read_re_executes_as_before() {
    // A main process gone, a unit not loaded, a binary since deleted.
    assert_eq!(value(None, Path::new(NEW), false), "1");
    assert_eq!(value(Some(Path::new(&format!("{NEW} (deleted)"))), Path::new(NEW), false), "1");
    // An activation the parent didn't tell, or told something else.
    assert!(wanted(None, false));
    assert!(wanted(Some(""), false));
    assert!(wanted(Some("yes"), false));
    // A config that's there but can't be read: a directory for a file.
    let root = dir("unreadable");
    std::fs::create_dir_all(root.join("old/user.conf")).unwrap();
    write(root.join("new/user.conf"), "[Manager]\n");
    assert!(config_changed(&root.join("old"), &root.join("new")));
    std::fs::remove_dir_all(root).unwrap();
}
