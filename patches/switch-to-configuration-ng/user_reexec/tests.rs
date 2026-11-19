//! user_reexec.rs's own: when a user manager is re-executed.

use super::*;

const NEW: &str = "/nix/store/new-systemd/lib/systemd/systemd";
const OLD: &str = "/nix/store/old-systemd/lib/systemd/systemd";

/// A fresh directory of this test's own.
fn dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("user-reexec-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(path: PathBuf, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// A user's config dir and the system's four, under one root.
struct Tree {
    root: PathBuf,
}

impl Tree {
    fn new(name: &str) -> Tree {
        Tree { root: dir(name) }
    }
    fn user(&self) -> PathBuf {
        self.root.join("home/.config/systemd")
    }
    fn system(&self, i: usize) -> PathBuf {
        self.root.join(SYSTEM_DIRS[i].trim_start_matches('/'))
    }
    fn print(&self) -> Option<String> {
        let system: Vec<PathBuf> = (0..4).map(|i| self.system(i)).collect();
        let system: Vec<&Path> = system.iter().map(PathBuf::as_path).collect();
        fingerprint(&self.user(), &system)
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn only_a_manager_on_the_new_binary_at_its_baseline_is_left_running() {
    let new = Path::new(NEW);
    assert!(!needed(Some(new), new, Some("a"), Some("a")));
    // Another binary, or one that can't be read (gone, deleted).
    assert!(needed(Some(Path::new(OLD)), new, Some("a"), Some("a")));
    assert!(needed(None, new, Some("a"), Some("a")));
    assert!(needed(Some(Path::new(&format!("{NEW} (deleted)"))), new, Some("a"), Some("a")));
    // A state that changed, can't be told, or has no baseline (a boot, a
    // re-execution that failed or wasn't seen).
    assert!(needed(Some(new), new, Some("b"), Some("a")));
    assert!(needed(Some(new), new, None, Some("a")));
    assert!(needed(Some(new), new, None, None));
    assert!(needed(Some(new), new, Some("a"), None));
}

#[test]
fn the_user_activation_skips_only_on_the_parents_say() {
    assert!(!wanted(Some("0")));
    assert!(wanted(Some("1")));
    assert!(wanted(None));
    assert!(wanted(Some("")));
}

#[test]
fn every_config_source_counts_added_changed_or_removed() {
    let tree = Tree::new("sources");
    let mut seen = vec![tree.print().unwrap()];
    let mut changed = |what: &str| {
        let now = tree.print().unwrap();
        assert!(!seen.contains(&now), "{what} left the fingerprint as it was");
        seen.push(now);
    };
    write(tree.system(0).join("user.conf"), "[Manager]\n");
    changed("/etc's user.conf added");
    write(tree.system(0).join("user.conf"), "[Manager]\nTimerSlackNSec=1ms\n");
    changed("/etc's user.conf changed");
    for i in 0..4 {
        write(tree.system(i).join("user.conf.d/a.conf"), "[Manager]\nTimerSlackNSec=2ms\n");
        changed(&format!("a drop-in under {}", SYSTEM_DIRS[i]));
    }
    // /run's masked by name, as systemd masks: a link to /dev/null.
    fs::remove_file(tree.system(1).join("user.conf.d/a.conf")).unwrap();
    std::os::unix::fs::symlink("/dev/null", tree.system(1).join("user.conf.d/a.conf")).unwrap();
    changed("/run's drop-in masked");
    write(tree.user().join("user.conf"), "[Manager]\n");
    changed("the user's own user.conf added");
    write(tree.user().join("user.conf.d/b.conf"), "[Manager]\nTimerSlackNSec=3ms\n");
    changed("the user's own drop-in added");
    fs::remove_file(tree.user().join("user.conf.d/b.conf")).unwrap();
    changed("the user's own drop-in removed");
    // A file systemd ignores (no .conf) still counts: nothing here models
    // what systemd reads of a directory.
    write(tree.user().join("user.conf.d/notes"), "x");
    changed("an entry of another name");
    // Nothing changed: the same fingerprint.
    assert_eq!(tree.print().unwrap(), *seen.last().unwrap());
}

#[test]
fn fifos_devices_and_links_are_told_without_opening_them() {
    let tree = Tree::new("special");
    let dropins = tree.user().join("user.conf.d");
    fs::create_dir_all(&dropins).unwrap();
    // A FIFO with no writer would block a read forever; /dev/zero never ends.
    nix::unistd::mkfifo(&dropins.join("fifo.conf"), nix::sys::stat::Mode::S_IRWXU).unwrap();
    std::os::unix::fs::symlink("/dev/zero", dropins.join("zero.conf")).unwrap();
    std::os::unix::fs::symlink(dropins.join("fifo.conf"), dropins.join("to-fifo.conf")).unwrap();
    std::os::unix::fs::symlink(tree.root.join("nowhere"), dropins.join("dangling.conf")).unwrap();
    fs::create_dir_all(dropins.join("sub.conf")).unwrap();
    let print = tree.print().unwrap();
    assert!(print.contains("fifo.conf\" fifo\n"), "{print}");
    assert!(print.contains("-> device true"), "{print}");
    assert!(print.contains("-> fifo\n"), "{print}");
    assert!(print.contains("-> dangling\n"), "{print}");
    assert!(print.contains("sub.conf\" dir\n"), "{print}");
}

#[test]
fn what_cant_be_told_has_no_fingerprint() {
    let tree = Tree::new("toolarge");
    let big = "#".repeat(MAX_BYTES as usize + 1);
    write(tree.system(0).join("user.conf"), &big);
    assert_eq!(tree.print(), None);
}

#[test]
fn the_config_dir_is_found_as_systemd_finds_it() {
    let env = |vars: &[&str]| vars.iter().flat_map(|v| [v.as_bytes(), b"\0"]).flatten().copied().collect::<Vec<u8>>();
    assert_eq!(config_dir(&env(&["HOME=/home/dev"])), Some("/home/dev/.config/systemd".into()));
    assert_eq!(
        config_dir(&env(&["HOME=/home/dev", "XDG_CONFIG_HOME=/x"])),
        Some("/x/systemd".into())
    );
    // A relative XDG_CONFIG_HOME is ignored, as systemd ignores it.
    assert_eq!(
        config_dir(&env(&["XDG_CONFIG_HOME=rel", "HOME=/home/dev"])),
        Some("/home/dev/.config/systemd".into())
    );
    // A key that only starts the same, and no usable one at all.
    assert_eq!(config_dir(&env(&["HOMEX=/y", "HOME=/home/dev"])), Some("/home/dev/.config/systemd".into()));
    assert_eq!(config_dir(&env(&["HOME=rel"])), None);
    assert_eq!(config_dir(&env(&[])), None);
}
