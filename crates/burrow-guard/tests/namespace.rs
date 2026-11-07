//! Where the guard takes its undo snapshot from: zfs directly in the
//! machine's own user namespace, the user's service manager outside it.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use burrow_guard::config::Config;
use burrow_guard::{Guard, Hook, Input, Namespace};
use tempfile::TempDir;

mod common;

/// A zfs that writes down what it's asked to do (and fails, saying `fail`,
/// when that file is there), and a user's service manager that writes down
/// what it's asked to run and runs it.
fn machine() -> (TempDir, Config) {
    let dir = tempfile::tempdir().unwrap();
    let zfs = common::zfs(dir.path());
    let systemd_run = common::systemd_run(dir.path());
    let config = Config {
        git: PathBuf::from("git"),
        zfs,
        systemd_run,
        snapshot_datasets: vec!["pool/a".to_owned(), "pool/b".to_owned()],
        ask_after_seconds: 8,
    };
    (dir, config)
}

fn lines(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// What the guard, in `namespace`, says about `rm -rf x`; each call a fresh
/// burst, so each takes its snapshot.
fn remove(dir: &Path, config: &Config, namespace: Namespace) -> Option<String> {
    let _ = fs::remove_file(dir.join("mark"));
    let guard = Guard {
        config,
        codex: true,
        mark: dir.join("mark"),
        namespace,
    };
    let hook = Hook {
        tool_name: Some("Bash".to_owned()),
        tool_input: Some(Input {
            command: Some("rm -rf x".to_owned()),
        }),
        cwd: Some(dir.to_string_lossy().into_owned()),
    };
    guard.check(&hook, SystemTime::now())
}

#[test]
fn outside_the_machines_namespace_the_users_manager_takes_the_snapshot() {
    let (dir, config) = machine();
    let (zfs_log, manager_log) = (dir.path().join("zfs.log"), dir.path().join("manager.log"));
    assert_eq!(remove(dir.path(), &config, Namespace::Other), None);
    let managed = lines(&manager_log);
    assert_eq!(managed.len(), 1, "{managed:?}");
    let words: Vec<&str> = managed[0].split(' ').collect();
    let zfs = config.zfs.display().to_string();
    assert_eq!(
        words[..8],
        [
            "--user",
            "--wait",
            "--pipe",
            "--quiet",
            "--collect",
            "--",
            &zfs,
            "snapshot"
        ],
    );
    assert!(words[8].starts_with("pool/a@pre-") && words[9].starts_with("pool/b@pre-"));
    // The manager ran zfs: the snapshot was taken, before the command runs.
    assert_eq!(lines(&zfs_log), [words[7..].join(" ")]);

    // What zfs said there comes back: with no snapshot, no undo.
    fs::write(
        dir.path().join("fail"),
        "cannot create snapshot: out of space\n",
    )
    .unwrap();
    assert_eq!(
        remove(dir.path(), &config, Namespace::Other).unwrap(),
        "Couldn't take the undo snapshot first (cannot create snapshot: out of space), so this would have no undo."
    );
    fs::remove_file(dir.path().join("fail")).unwrap();

    // In the machine's own, zfs runs directly.
    assert_eq!(remove(dir.path(), &config, Namespace::Machine), None);
    assert_eq!(lines(&zfs_log).len(), 3);
    assert_eq!(lines(&manager_log).len(), 2, "zfs went through the manager");
}

#[test]
fn the_namespace_is_read_from_the_uid_map() {
    // The machine's own, as /proc/self/uid_map shows it there.
    assert_eq!(
        Namespace::from_uid_map("         0          0 4294967295\n"),
        Namespace::Machine
    );
    // An unprivileged bwrap's, a container's, one short of the whole range,
    // and one with no map yet.
    for map in [
        "      1001       1001          1\n",
        "         0       1001          1\n",
        "         0     100000      65536\n",
        "         0          0 4294967294\n",
        "",
    ] {
        assert_eq!(Namespace::from_uid_map(map), Namespace::Other, "{map:?}");
    }
}
