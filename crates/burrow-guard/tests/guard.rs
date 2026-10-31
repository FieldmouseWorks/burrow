//! The guard's controls: each would fail on the bug it's for.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

use burrow_guard::config::Config;
use burrow_guard::{Guard, Hook, Input};
use tempfile::TempDir;

/// A machine for the guard: a zfs that writes down what it's asked to do
/// (and fails, saying `fail`, when that file is there), two undo datasets,
/// and the mark in a runtime dir of its own.
struct Box {
    dir: TempDir,
    config: Config,
}

impl Box {
    fn new() -> Box {
        Box::with_datasets(&["pool/a", "pool/b"])
    }

    fn with_datasets(datasets: &[&str]) -> Box {
        let dir = tempfile::tempdir().unwrap();
        let zfs = dir.path().join("zfs");
        fs::write(
            &zfs,
            format!(
                "#!/bin/sh\necho \"$@\" >> '{0}/zfs.log'\n[ ! -e '{0}/fail' ] || {{ cat '{0}/fail' >&2; exit 1; }}\n",
                dir.path().display()
            ),
        )
        .unwrap();
        fs::set_permissions(&zfs, fs::Permissions::from_mode(0o755)).unwrap();
        let config = Config {
            git: PathBuf::from("git"),
            zfs,
            snapshot_datasets: datasets.iter().map(|d| (*d).to_owned()).collect(),
        };
        Box { dir, config }
    }

    fn mark(&self) -> PathBuf {
        self.dir.path().join("claude-guard-last-snapshot")
    }

    /// What the guard says about `cmd` run in `cwd`, now.
    fn check(&self, cmd: &str, cwd: &Path) -> Option<String> {
        self.check_at(cmd, cwd, SystemTime::now())
    }

    fn check_at(&self, cmd: &str, cwd: &Path, now: SystemTime) -> Option<String> {
        let guard = Guard {
            config: &self.config,
            mark: self.mark(),
        };
        guard.check(&bash(cmd, cwd), now)
    }

    /// The snapshot rounds zfs was asked for.
    fn snapshots(&self) -> Vec<String> {
        fs::read_to_string(self.dir.path().join("zfs.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// A fresh burst: no snapshot taken yet.
    fn forget(&self) {
        let _ = fs::remove_file(self.dir.path().join("zfs.log"));
        let _ = fs::remove_file(self.mark());
    }
}

fn bash(cmd: &str, cwd: &Path) -> Hook {
    Hook {
        tool_name: Some("Bash".to_owned()),
        tool_input: Some(Input {
            command: Some(cmd.to_owned()),
        }),
        cwd: Some(cwd.to_string_lossy().into_owned()),
    }
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args([
            "-c",
            "user.name=Invented",
            "-c",
            "user.email=invented@example.invalid",
        ])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdout(Stdio::null())
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

/// A clone on `branch`, tracking origin's `branch`.
fn clone_on(branch: &str) -> TempDir {
    clone_tracking(branch, branch)
}

/// A clone on `branch`, tracking origin's `upstream`.
fn clone_tracking(branch: &str, upstream: &str) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    git(d, &["init", "-q", "-b", branch]);
    git(d, &["commit", "-q", "--allow-empty", "-m", "invented"]);
    git(d, &["remote", "add", "origin", "/dev/null"]);
    git(
        d,
        &[
            "update-ref",
            &format!("refs/remotes/origin/{upstream}"),
            "HEAD",
        ],
    );
    git(d, &["config", &format!("branch.{branch}.remote"), "origin"]);
    git(
        d,
        &[
            "config",
            &format!("branch.{branch}.merge"),
            &format!("refs/heads/{upstream}"),
        ],
    );
    dir
}

const SHARED: &str = "(R2: rewrites history other clones depend on).";

#[test]
fn only_bash_is_read() {
    let b = Box::new();
    let mut hook = bash("rm -rf / && git push -f origin main", Path::new("/"));
    hook.tool_name = Some("Write".to_owned());
    let guard = Guard {
        config: &b.config,
        mark: b.mark(),
    };
    assert_eq!(guard.check(&hook, SystemTime::now()), None);
    assert!(b.snapshots().is_empty());
}

#[test]
fn what_destroys_files_is_snapshotted_first() {
    let b = Box::new();
    let at = Path::new("/");
    for cmd in [
        "rm -rf target",
        "rmdir x",
        "shred x",
        "truncate -s 0 x",
        "unlink x",
        "git clean -fdx",
        "git reset --hard",
        "git checkout -- .",
        "git switch main",
        "git restore x",
        "git stash",
        "git branch -D x",
        "git worktree remove /tmp/x",
        "git filter-repo --path x",
        "git -C /tmp/x reset --hard",
        "git -c core.x=y reset --hard",
        "echo x > out.txt",
        "echo x >> out.txt",
        "echo x>out",
        "echo x > /tmp/out",
        "echo x 2> err.log",
        "cmd 2>>err.log",
        "cmd &> all.log",
        "x 1>out",
        "rsync -a --delete a/ b/",
        "find . -name '*.o' -delete",
        "mv a b",
        "cp a b",
        "echo x | tee f",
        "sed -i s/a/b/ f",
        "perl -pi -e s/a/b/ f",
        "(rm x)",
        "true && rm x",
        "true;rm x",
        "bash -c 'rm -rf x'",
        "sh -c \"cp a b\"",
        "echo `rm x`",
        "echo $(rm x)",
        "/bin/rm x",
        "/usr/bin/mv a b",
        "ssh host 'rm -rf /data/x'",
        "echo a\nrm b",
    ] {
        b.forget();
        assert_eq!(b.check(cmd, at), None, "{cmd:?}");
        let snaps = b.snapshots();
        assert_eq!(snaps.len(), 1, "{cmd:?}: {snaps:?}");
        let words: Vec<&str> = snaps[0].split(' ').collect();
        assert_eq!(words.len(), 3, "{snaps:?}");
        assert_eq!(words[0], "snapshot");
        let stamp = words[1].strip_prefix("pool/a@pre-").unwrap();
        assert_eq!(
            words[2],
            format!("pool/b@pre-{stamp}"),
            "one round, one name"
        );
        assert!(stamp.len() == 15 && stamp.as_bytes()[8] == b'-', "{stamp}");
    }
}

#[test]
fn what_leaves_files_alone_isnt() {
    let b = Box::new();
    for cmd in [
        "ls -la",
        "cat file.txt",
        "git status",
        "git log --oneline -5",
        "cargo test 2>/dev/null",
        "make 2>&1 | tail",
        "ls >/dev/null",
        "cmd &>/dev/null",
        "cargo fmt",
        "echo x >&2",
        "cmd 2>&1",
        "git push origin feature",
        "",
    ] {
        b.forget();
        assert_eq!(b.check(cmd, Path::new("/")), None, "{cmd:?}");
        assert!(b.snapshots().is_empty(), "{cmd:?}");
    }
}

#[test]
fn one_round_of_snapshots_per_burst() {
    let b = Box::new();
    let at = Path::new("/");
    let now = SystemTime::now();
    b.check_at("rm a", at, now);
    b.check_at("rm b", at, now + Duration::from_secs(4));
    assert_eq!(b.snapshots().len(), 1, "a second round inside the burst");
    b.check_at("rm c", at, now + Duration::from_secs(5));
    assert_eq!(b.snapshots().len(), 2, "no round once the burst is over");
    // A mark from the future vouches for nothing.
    fs::File::options()
        .write(true)
        .open(b.mark())
        .unwrap()
        .set_modified(now + Duration::from_secs(3600))
        .unwrap();
    b.check_at("rm d", at, now + Duration::from_secs(6));
    assert_eq!(b.snapshots().len(), 3, "a mark from the future held it off");
}

#[test]
fn no_snapshot_no_undo_so_it_asks() {
    let b = Box::new();
    fs::write(
        b.dir.path().join("fail"),
        "cannot create snapshot: out of space\n",
    )
    .unwrap();
    let asked = b.check("rm -rf x", Path::new("/")).unwrap();
    assert_eq!(
        asked,
        "Couldn't take the undo snapshot first (cannot create snapshot: out of space), so this would have no undo."
    );
    // A failed round doesn't hold off the next: it's tried again.
    assert!(b.check("rm -rf x", Path::new("/")).is_some());
    assert_eq!(b.snapshots().len(), 2);

    let mut gone = Box::new();
    gone.config.zfs = gone.dir.path().join("no-such-zfs");
    let asked = gone.check("rm -rf x", Path::new("/")).unwrap();
    assert!(
        asked.starts_with("Couldn't take the undo snapshot first ("),
        "{asked}"
    );
}

#[test]
fn no_datasets_no_snapshot() {
    let b = Box::with_datasets(&[]);
    assert_eq!(b.check("rm -rf x", Path::new("/")), None);
    assert!(b.snapshots().is_empty());
}

#[test]
fn a_push_that_can_rewrite_a_shared_branch_asks() {
    let b = Box::new();
    let main = clone_on("main");
    let feature = clone_on("feature");
    let (m, f) = (main.path(), feature.path());
    let in_main = format!("git -C {} push -f", m.display());
    for (cmd, at, branch) in [
        ("git push -f", m, "main"),
        ("git push --force", m, "main"),
        ("git push --force-with-lease", m, "main"),
        ("git push --force origin main", f, "main"),
        (
            "git push --force-with-lease=main:abc origin main",
            f,
            "main",
        ),
        ("git push --force-if-includes origin main", f, "main"),
        ("git push origin +main", f, "main"),
        ("git push origin :main", f, "main"),
        ("git push origin --delete main", f, "main"),
        ("git push -f origin HEAD", f, "HEAD"),
        ("git push -f origin @", f, "@"),
        ("git push -f origin HEAD:refs/heads/main", f, "main"),
        ("git push -f origin feature:main", f, "main"),
        ("git push -f origin refs/heads/main", f, "main"),
        ("git push -f origin feature main", f, "main"),
        ("git push -f origin master", f, "master"),
        ("git push -f origin trunk", f, "trunk"),
        (&in_main, f, "main"),
        ("cd x && git push -f origin main", f, "main"),
        ("echo git push -f origin main", f, "main"),
        ("git fetch && git push --force", m, "main"),
        // Every push in the command, not only the first.
        (
            "git push -f origin feature && git push -f origin main",
            f,
            "main",
        ),
        ("git push origin feature; git push -f", m, "main"),
        // Short options together.
        ("git push -uf origin main", f, "main"),
        ("git push -fu", m, "main"),
        // Quotes don't hide a refspec.
        ("git push -f origin \"+main\"", f, "main"),
        ("git push origin \"+main\"", f, "main"),
        ("git push -f 'origin' 'main'", f, "main"),
        ("ssh remi 'git -C /data push -f origin main'", f, "main"),
        // Options before push, and option values that aren't the remote.
        ("git -c push.default=current push -f", m, "main"),
        ("git push -o ci.skip -f origin main", f, "main"),
        ("git push -f -o ci.skip origin", m, "main"),
        ("git push -f --push-option ci.skip origin", m, "main"),
    ] {
        assert_eq!(
            b.check(cmd, at),
            Some(format!("Force-push or delete of '{branch}' {SHARED}")),
            "{cmd:?}"
        );
    }
    // With no refspec, the branch checked out and its upstream both count.
    let onto_main = clone_tracking("work", "main");
    let from_main = clone_tracking("main", "work");
    for at in [onto_main.path(), from_main.path()] {
        assert_eq!(
            b.check("git push -f", at),
            Some(format!("Force-push or delete of 'main' {SHARED}")),
            "{at:?}"
        );
    }
    let relative = format!(
        "git -C ../{} push -f",
        m.file_name().unwrap().to_str().unwrap()
    );
    assert_eq!(
        b.check(&relative, f),
        Some(format!("Force-push or delete of 'main' {SHARED}")),
        "-C is taken from where the command runs"
    );
    for cmd in [
        "git push --all -f",
        "git push --branches -f",
        "git push --mirror",
        "git push --prune origin",
    ] {
        assert_eq!(
            b.check(cmd, f),
            Some(format!(
                "Push that can rewrite or delete every remote branch {SHARED}"
            )),
            "{cmd:?}"
        );
    }
}

#[test]
fn a_push_that_cant_rewrite_a_shared_branch_doesnt_ask() {
    let b = Box::new();
    let main = clone_on("main");
    let feature = clone_on("feature");
    let (m, f) = (main.path(), feature.path());
    let in_feature = format!("git -C {} push -f", f.display());
    for (cmd, at) in [
        ("git push", m),
        ("git push origin main", m),
        ("git push --all", f),
        ("git push -f", f),
        ("git push -f origin feature", m),
        ("git push origin +feature", m),
        ("git push -d origin feature", m),
        ("git push -f origin main:feature", f),
        (&in_feature, m),
        ("git push --dry-run origin main", f),
        ("git push -o fd origin main", f),
        ("git push -ofd origin", m),
    ] {
        assert_eq!(b.check(cmd, at), None, "{cmd:?}");
    }
}

#[test]
fn firmware_and_robot_calls_with_no_undo_ask() {
    let b = Box::with_datasets(&[]);
    let at = Path::new("/");
    let boot = "Changes UEFI boot entries (R2: a bad one can stop the box booting).";
    let vars = "Writes EFI variables (R2: a bad one can stop the box booting).";
    let robot = "Hetzner Robot call that cancels, orders or reinstalls a server (R2: no undo).";
    for (cmd, asked) in [
        ("efibootmgr -o 0001,0002", Some(boot)),
        ("efibootmgr --bootnext 0001", Some(boot)),
        ("efibootmgr -c -d /dev/sda", Some(boot)),
        ("sudo efibootmgr -n 1", Some(boot)),
        ("efibootmgr -v -B -b 0003", Some(boot)),
        ("efibootmgr", None),
        ("efibootmgr -v", None),
        ("rm /sys/firmware/efi/efivars/x", Some(vars)),
        ("chattr -i /sys/firmware/efi/efivars/x", Some(vars)),
        ("echo x > /sys/firmware/efi/efivars/x", Some(vars)),
        ("ls /sys/firmware/efi/efivars", None),
        ("cat /sys/firmware/efi/efivars/Boot0001-x", None),
        (
            "curl -X POST https://robot-ws.your-server.de/server/1/cancellation",
            Some(robot),
        ),
        (
            "curl -X POST https://robot-ws.your-server.de/boot/1/linux -d dist=x",
            Some(robot),
        ),
        (
            "curl https://robot-ws.your-server.de/order/server/product",
            Some(robot),
        ),
        ("curl https://robot-ws.your-server.de/server", None),
        ("curl https://robot-ws.your-server.de/boot/1/rescue", None),
    ] {
        assert_eq!(b.check(cmd, at).as_deref(), asked, "{cmd:?}");
    }
}

/// A command the owner lets through at a question still has its undo, and
/// the question names every door it opens.
#[test]
fn the_undo_comes_before_the_question() {
    let b = Box::new();
    let feature = clone_on("feature");
    let asked = b
        .check(
            "rm -rf x && efibootmgr -o 1; git push -f origin main",
            feature.path(),
        )
        .unwrap();
    assert_eq!(
        b.snapshots().len(),
        1,
        "no undo for a command it asked about"
    );
    assert_eq!(
        asked,
        format!(
            "Force-push or delete of 'main' {SHARED} Changes UEFI boot entries (R2: a bad one can stop the box booting)."
        )
    );
}
