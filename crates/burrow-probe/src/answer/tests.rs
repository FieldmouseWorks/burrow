use std::cell::RefCell;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use super::{Machine, Ran, Run, size};
use crate::query::Query;
use crate::{Config, Journal, Tools};

/// A stand-in for the commands: what each was asked, and one answer for a
/// program and its first argument.
#[derive(Default)]
struct Fake {
    asked: RefCell<Vec<(String, Vec<String>, bool)>>,
    answers: Vec<(String, String, Ran)>,
}

impl Fake {
    fn on(mut self, program: &str, first: &str, code: i32, stdout: &str) -> Self {
        self.answers.push((
            program.into(),
            first.into(),
            Ran {
                code: Some(code),
                stdout: stdout.into(),
                stderr: String::new(),
            },
        ));
        self
    }
}

impl Run for Fake {
    fn run(&self, program: &Path, args: &[&str], user: bool, _uid: u32) -> Result<Ran, String> {
        let name = program.file_name().unwrap().to_string_lossy().into_owned();
        self.asked.borrow_mut().push((
            name.clone(),
            args.iter().map(|s| s.to_string()).collect(),
            user,
        ));
        let first = args.iter().find(|a| **a != "--user").copied().unwrap_or("");
        self.answers
            .iter()
            .find(|(p, f, _)| *p == name && *f == first)
            .map(|(_, _, r)| Ran {
                code: r.code,
                stdout: r.stdout.clone(),
                stderr: r.stderr.clone(),
            })
            .ok_or_else(|| format!("{name} {first}: not scripted"))
    }
}

fn config(state: &Path) -> Config {
    Config {
        repo: "sam/box".into(),
        label: "probe".into(),
        authors: vec!["sam".into()],
        host: "box".into(),
        user: "dev".into(),
        journal: Journal {
            units: vec!["box-update.service".into(), "autoland-*.service".into()],
            user_units: vec!["claude-rc@*.service".into()],
        },
        state_dir: state.to_path_buf(),
        proj: Some(PathBuf::from("/run/current-system/sw/bin/proj")),
        tools: Tools {
            journalctl: "/bin/journalctl".into(),
            systemctl: "/bin/systemctl".into(),
        },
    }
}

fn machine<'a>(cfg: &'a Config, root: &Path, run: &'a Fake) -> Machine<'a> {
    Machine {
        cfg,
        root: root.to_path_buf(),
        now: 1_793_500_000,
        uid: 1001,
        run,
    }
}

fn write(root: &Path, path: &str, text: &str) {
    let p = root.join(path.trim_start_matches('/'));
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

#[test]
fn sizes() {
    assert_eq!(size(512 * 1024), "512K");
    assert_eq!(size(300 * 1024 * 1024), "300M");
    assert_eq!(size(13 * 1024 * 1024 * 1024 / 2), "6.5G");
}

#[test]
fn memory_lists_the_biggest_cgroups() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "/proc/meminfo",
        "MemTotal: 65536000 kB\nMemAvailable: 32768000 kB\nSwapTotal: 16384000 kB\nSwapFree: 16000000 kB\n",
    );
    write(
        root,
        "/proc/pressure/memory",
        "some avg10=1.00 avg60=0.50 avg300=0.10 total=1\nfull avg10=0.00 avg60=0.00 avg300=0.00 total=0\n",
    );
    write(
        root,
        "/proc/spl/kstat/zfs/arcstats",
        "name type data\nc 4 8589934592\nc_max 4 8589934592\nsize 4 4294967296\n",
    );
    let cg = "/sys/fs/cgroup";
    write(
        root,
        &format!("{cg}/system.slice/memory.current"),
        "1073741824\n",
    );
    write(
        root,
        &format!("{cg}/system.slice/nix-daemon.service/memory.current"),
        "536870912\n",
    );
    write(
        root,
        &format!("{cg}/system.slice/nix-daemon.service/memory.peak"),
        "2147483648\n",
    );
    let rc = format!(
        "{cg}/user.slice/user-1001.slice/user@1001.service/claude.slice/claude-rc.slice/claude-rc@web.service"
    );
    write(root, &format!("{rc}/memory.current"), "3221225472\n");
    write(root, &format!("{rc}/memory.swap.current"), "0\n");
    let (cfg, fake) = (config(root), Fake::default());
    let out = machine(&cfg, root, &fake).answer(&Query::Memory).unwrap();
    assert!(
        out.starts_with("RAM 62.5G total, 31.2G available. Swap 15.6G total, 15.3G free.\n"),
        "{out}"
    );
    assert!(
        out.contains("ZFS ARC 4.0G (target 8.0G, most 8.0G)."),
        "{out}"
    );
    assert!(out.contains("  some avg10=1.00"), "{out}");
    let rows: Vec<&str> = out
        .lines()
        .skip_while(|l| !l.starts_with("The 20"))
        .skip(1)
        .collect();
    assert_eq!(rows.len(), 3, "{out}");
    assert!(
        rows[0]
            .trim_start()
            .starts_with("3.0G       -      0K  user.slice/"),
        "{out}"
    );
    assert!(rows[0].ends_with("claude-rc@web.service"), "{out}");
    assert!(rows[1].ends_with("  system.slice"), "{out}");
    assert!(
        rows[2].contains("512M    2.0G       -  system.slice/nix-daemon.service"),
        "{out}"
    );
}

#[test]
fn a_journal_on_the_allowlist() {
    let dir = tempfile::tempdir().unwrap();
    let (cfg, fake) = (
        config(dir.path()),
        Fake::default()
            .on("journalctl", "-u", 0, "line one\nline two\n")
            .on("journalctl", "--user-unit", 0, ""),
    );
    let m = machine(&cfg, dir.path(), &fake);
    let out = m
        .answer(&Query::parse("journal box-update -2h now").unwrap())
        .unwrap();
    assert!(
        out.starts_with(
            "box-update.service, 2026-11-01 00:26:40Z to 2026-11-01 02:26:40Z: 2 lines\nline one\n"
        ),
        "{out}"
    );
    let asked = fake.asked.borrow();
    let (_, args, user) = &asked[0];
    assert!(!user);
    assert_eq!(
        args.join(" "),
        "-u box-update.service --since @1793492800 --until @1793500000.999999 -n 400 -o short-iso --no-pager --no-hostname -q --utc"
    );
    drop(asked);
    // A user unit, by its own glob.
    m.answer(&Query::parse(r"journal claude-rc@LoK\x2dweb -1h -30m").unwrap())
        .unwrap();
    assert_eq!(
        fake.asked.borrow()[1].1[..3],
        [
            "--user-unit".to_string(),
            r"claude-rc@LoK\x2dweb.service".to_string(),
            "--since".to_string(),
        ]
    );
}

#[test]
fn a_journal_on_both_allowlists_is_read_from_both_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(dir.path());
    cfg.journal.units.push("proj-*.service".into());
    cfg.journal.user_units.push("proj-*.service".into());
    let fake = Fake::default().on("journalctl", "-u", 0, "a job's line\n");
    let m = machine(&cfg, dir.path(), &fake);
    let out = m
        .answer(&Query::parse("journal proj-web-1 -1h now").unwrap())
        .unwrap();
    assert!(out.contains(": 1 lines\na job's line\n"), "{out}");
    let asked = fake.asked.borrow();
    assert_eq!(
        asked.len(),
        1,
        "one read, so the span and the limit are shared"
    );
    assert_eq!(
        asked[0].1.join(" "),
        "-u proj-web-1.service --user-unit proj-web-1.service --since @1793496400 --until @1793500000.999999 -n 400 -o short-iso --no-pager --no-hostname -q --utc"
    );
}

#[test]
fn a_journal_off_the_allowlist_is_never_read() {
    let dir = tempfile::tempdir().unwrap();
    let (cfg, fake) = (config(dir.path()), Fake::default());
    let m = machine(&cfg, dir.path(), &fake);
    for line in [
        "journal sshd -1h now",
        "journal claude-rc@web.timer -1h now",
        "journal box-update -1h -2h",
    ] {
        let e = m.answer(&Query::parse(line).unwrap()).unwrap_err();
        assert!(
            e.contains("allowlist") || e.contains("isn't before"),
            "{line}: {e}"
        );
    }
    assert!(fake.asked.borrow().is_empty());
}

#[test]
fn rc_names_each_server() {
    let dir = tempfile::tempdir().unwrap();
    let list = "claude-rc@remi.service loaded active running remi\nclaude-rc@LoK\\x2dweb.service loaded failed failed LoK-web\n";
    let show = "Id=claude-rc@remi.service\nActiveState=active\nSubState=running\nMemoryCurrent=1073741824\nMemoryPeak=2147483648\nNRestarts=0\nActiveEnterTimestamp=Sun 2026-11-01 13:05:01 UTC\n\nId=claude-rc@LoK\\x2dweb.service\nActiveState=failed\nSubState=failed\nMemoryCurrent=18446744073709551615\nMemoryPeak=[not set]\nNRestarts=3\nActiveEnterTimestamp=\n";
    let (cfg, fake) = (
        config(dir.path()),
        Fake::default()
            .on("systemctl", "list-units", 0, list)
            .on("systemctl", "show", 0, show),
    );
    let out = machine(&cfg, dir.path(), &fake).answer(&Query::Rc).unwrap();
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[0].starts_with("2 Remote Control servers"), "{out}");
    assert!(
        lines[1]
            .split_whitespace()
            .collect::<Vec<_>>()
            .starts_with(&["remi", "active/running", "1.0G", "2.0G", "0"]),
        "{out}"
    );
    assert_eq!(
        lines[2].split_whitespace().collect::<Vec<_>>(),
        ["LoK-web", "failed/failed", "-", "-", "3", "-"],
        "{out}"
    );
    let asked = fake.asked.borrow();
    assert!(
        asked
            .iter()
            .all(|(_, args, user)| *user && args[0] == "--user")
    );
    assert_eq!(
        asked[1].1[4..],
        [
            "claude-rc@remi.service".to_string(),
            r"claude-rc@LoK\x2dweb.service".to_string()
        ]
    );
}

#[test]
fn deployed_says_what_runs() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("run")).unwrap();
    fs::create_dir_all(root.join("nix/var/nix/profiles")).unwrap();
    symlink(
        "/nix/store/aaa-nixos-system-box",
        root.join("run/current-system"),
    )
    .unwrap();
    symlink(
        "/nix/store/bbb-nixos-system-box",
        root.join("run/booted-system"),
    )
    .unwrap();
    symlink("system-12-link", root.join("nix/var/nix/profiles/system")).unwrap();
    symlink(
        "/nix/store/aaa-nixos-system-box",
        root.join("nix/var/nix/profiles/system-12-link"),
    )
    .unwrap();
    write(
        root,
        "/etc/burrow/inputs.json",
        r#"{"burrow": {"rev": "abc123", "lastModified": 1793400000}}"#,
    );
    write(
        root,
        "/state/live.json",
        r#"{"main": "def456", "running": "/nix/store/aaa-nixos-system-box"}"#,
    );
    let (cfg, fake) = (config(&root.join("state")), Fake::default());
    let out = machine(&cfg, root, &fake).answer(&Query::Deployed).unwrap();
    assert!(out.starts_with("Running: /nix/store/aaa-nixos-system-box\nBooted: /nix/store/bbb-nixos-system-box\nGeneration: system-12-link, made "), "{out}");
    assert!(out.contains("NixOS version: nixos-version"), "{out}");
    assert!(
        out.contains("  burrow           abc123  2026-10-30 22:40:00Z\n"),
        "{out}"
    );
    assert!(out.ends_with("The update last found main at def456 live, in /nix/store/aaa-nixos-system-box, which is what runs\n"), "{out}");
}

#[test]
fn oom_reads_the_kernel_and_oomd() {
    let dir = tempfile::tempdir().unwrap();
    let (cfg, fake) = (
        config(dir.path()),
        Fake::default()
            .on(
                "journalctl",
                "_TRANSPORT=kernel",
                0,
                "2026-11-01T01:00:00+00:00 kernel: Out of memory: Killed process 42 (nix)\n",
            )
            .on("journalctl", "-u", 1, ""),
    );
    let out = machine(&cfg, dir.path(), &fake)
        .answer(&Query::parse("oom").unwrap())
        .unwrap();
    assert!(
        out.contains("Killed process 42 (nix)\n\nsystemd-oomd's:\nnone\n"),
        "{out}"
    );
    // Every boot in the span, the kernel's as well as oomd's: no `-k` or
    // `-b`, which mean this boot only.
    for (_, args, _) in fake.asked.borrow().iter() {
        assert!(
            !args.iter().any(|a| a == "-k"
                || a == "--dmesg"
                || a.starts_with("-b")
                || a.starts_with("--boot")),
            "{args:?}"
        );
    }
}

#[test]
fn units_and_queue_ask_the_user_manager_too() {
    let dir = tempfile::tempdir().unwrap();
    let (cfg, fake) = (
        config(dir.path()),
        Fake::default()
            .on("systemctl", "list-units", 0, "")
            .on("systemctl", "list-jobs", 0, "")
            .on("proj", "status", 0, "job-1 web running\n"),
    );
    let m = machine(&cfg, dir.path(), &fake);
    let out = m.answer(&Query::Units).unwrap();
    assert_eq!(
        out,
        "The system, failed units:\nnone\nThe system, jobs under way:\nnone\ndev's user manager, failed units:\nnone\ndev's user manager, jobs under way:\nnone\n"
    );
    assert_eq!(m.answer(&Query::Queue).unwrap(), "job-1 web running\n");
    let asked = fake.asked.borrow();
    let users: Vec<bool> = asked.iter().map(|a| a.2).collect();
    assert_eq!(users, [false, false, true, true, true]);
}
