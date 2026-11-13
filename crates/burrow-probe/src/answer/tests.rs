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
        jobs: None,
        projects: None,
        machinery: None,
        repo: "sam/box".into(),
        label: "probe".into(),
        authors: vec![burrow_github::fake::author("sam")],
        host: "box".into(),
        user: "dev".into(),
        journal: Journal {
            units: vec!["box-update.service".into(), "autoland-*.service".into()],
            user_units: vec!["claude-rc@*.service".into()],
        },
        state_dir: state.to_path_buf(),
        system_repo: None,
        proj: Some(PathBuf::from("/run/current-system/sw/bin/proj")),
        tools: Tools {
            git: "git".into(),
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
fn crash_reports_use_the_host_journal_and_only_diagnostic_fields() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let entry = serde_json::json!({
        "COREDUMP_EXE": "/nix/store/example-godot/libexec/godot",
        "COREDUMP_PID": "2168633",
        "COREDUMP_SIGNAL": "6",
        "COREDUMP_USER_UNIT": "proj-example.service",
        "COREDUMP_CGROUP": "/user.slice/proj-example.service",
        "MESSAGE": "Process dumped core.\nStack trace of thread 2168633:\n#0 abort (libc.so.6)",
        "COREDUMP_ENVIRON": "PRIVATE_ENV_VALUE",
        "COREDUMP_CMDLINE": "PRIVATE_ARGUMENT_VALUE",
        "COREDUMP_PROC_MAPS": "PRIVATE_MEMORY_MAP",
        "__CURSOR": "PRIVATE_CURSOR"
    })
    .to_string();
    let fake = Fake::default().on(
        "journalctl",
        "MESSAGE_ID=fc2e22bc6ee647b6b90729ab34a250b1",
        0,
        &entry,
    );
    let q = Query::parse("crashes /nix/store/example-godot/libexec/godot -1h").unwrap();
    let got = machine(&cfg, dir.path(), &fake).answer(&q).unwrap();
    assert!(got.contains("COREDUMP_PID: 2168633"));
    assert!(got.contains("COREDUMP_SIGNAL: 6"));
    assert!(got.contains("proj-example.service"));
    assert!(got.contains("#0 abort (libc.so.6)"));
    assert!(!got.contains("PRIVATE_"));
    let asked = fake.asked.borrow();
    let (_, args, user) = &asked[0];
    assert!(!user);
    assert!(args.contains(&"COREDUMP_EXE=/nix/store/example-godot/libexec/godot".into()));
    assert!(args.contains(&"@1793496400".into()));
    assert!(args.contains(&"--reverse".into()));
    assert!(args.windows(2).any(|a| a == ["-n", "5"]));
    assert!(!args.iter().any(|a| a.contains("COREDUMP_ENVIRON")));
}

#[test]
fn crash_queries_reject_commands_and_report_an_empty_span() {
    for text in [
        "crashes",
        "crashes godot",
        "crashes --all",
        "crashes /bin/*",
        "crashes /bin/../secret",
        "crashes /bin/godot;id",
        "crashes /bin/godot --all",
        "crashes /bin/godot -1h now",
        "crashes /",
        "crashes /bin//godot",
    ] {
        assert!(Query::parse(text).is_err(), "{text}");
    }
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let fake = Fake::default().on(
        "journalctl",
        "MESSAGE_ID=fc2e22bc6ee647b6b90729ab34a250b1",
        0,
        "",
    );
    let q = Query::parse("crashes /bin/godot").unwrap();
    assert_eq!(q.to_string(), "crashes /bin/godot -1d");
    assert!(
        machine(&cfg, dir.path(), &fake)
            .answer(&q)
            .unwrap()
            .ends_with("none\n")
    );
    let bad = Query::parse("crashes /bin/godot now").unwrap();
    assert!(machine(&cfg, dir.path(), &fake).answer(&bad).is_err());
    assert_eq!(fake.asked.borrow().len(), 1);
}

#[test]
fn crash_reports_keep_complete_fields_and_reject_bad_journal_results() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let q = Query::parse("crashes /bin/godot").unwrap();
    for bad in ["not json", r#"{"COREDUMP_EXE":"/bin/other"}"#] {
        let fake = Fake::default().on(
            "journalctl",
            "MESSAGE_ID=fc2e22bc6ee647b6b90729ab34a250b1",
            0,
            bad,
        );
        assert!(machine(&cfg, dir.path(), &fake).answer(&q).is_err());
    }
    let fake = Fake::default().on(
        "journalctl",
        "MESSAGE_ID=fc2e22bc6ee647b6b90729ab34a250b1",
        1,
        "",
    );
    assert!(machine(&cfg, dir.path(), &fake).answer(&q).is_err());
    let entry =
        serde_json::json!({"COREDUMP_EXE":"/bin/godot", "MESSAGE":"x".repeat(12001)}).to_string();
    let fake = Fake::default().on(
        "journalctl",
        "MESSAGE_ID=fc2e22bc6ee647b6b90729ab34a250b1",
        0,
        &entry,
    );
    let got = machine(&cfg, dir.path(), &fake).answer(&q).unwrap();
    assert!(got.contains(&"x".repeat(12001)));
}

#[test]
fn crash_comments_scrub_credentials_across_old_field_boundaries() {
    use burrow_github::fake::{Fake as Github, actor, ok};
    use serde_json::json;
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    for (field, limit) in [("COREDUMP_CGROUP", 1024), ("MESSAGE", 12000)] {
        for credential in [
            "reader:opaque-value".to_string(),
            format!("gh{}_{}", "p", "x".repeat(36)),
        ] {
            // The URL's @ or the token's twentieth payload character falls
            // just beyond the old cut, defeating recognition after cutting.
            let (value, hidden) = if credential.starts_with("reader:") {
                let url = format!("https://{credential}");
                (
                    format!(
                        "Authorization: {}\n{url}@example.org/path",
                        "x".repeat(limit - url.len() - 16)
                    ),
                    credential,
                )
            } else {
                (
                    format!(
                        "Authorization: {}\n{credential} end",
                        "x".repeat(limit - 4 - 19 - 16)
                    ),
                    credential[..23].to_string(),
                )
            };
            let mut entry = json!({"COREDUMP_EXE":"/bin/example", "COREDUMP_PID":"123"});
            entry[field] = json!(value);
            let fake = Fake::default().on(
                "journalctl",
                "MESSAGE_ID=fc2e22bc6ee647b6b90729ab34a250b1",
                0,
                &entry.to_string(),
            );
            let m = machine(&cfg, dir.path(), &fake);
            let q = Query::parse("crashes /bin/example -1h").unwrap();
            assert!(
                m.answer(&q).unwrap().contains(&hidden),
                "local ask remains unscrubbed"
            );
            let body = "crashes /bin/example -1h";
            let author = actor("sam");
            let mut gh = Github::default();
            gh.on("GET", "repos/sam/box/issues", vec![ok(200, json!([{"number":1,"user":author,"title":"probe","body":body}]), &[])])
                .on("GET", "repos/sam/box", vec![ok(200, json!({"private":true}), &[])])
                .on("POST", "graphql", vec![ok(200, json!({"data":{"repository":{"issue":{
                    "author": {"login":"sam","databaseId":author["id"]}, "title":"probe", "body":body,
                    "userContentEdits":{"totalCount":0,"nodes":[]}, "timelineItems":{"totalCount":0,"nodes":[]}
                }}}}), &[])])
                .on("POST", "repos/sam/box/issues/1/comments", vec![ok(201,json!({}), &[])])
                .on("PATCH", "repos/sam/box/issues/1", vec![ok(200,json!({}), &[])]);
            crate::pass::pass(&mut gh, &cfg, &mut |q| m.answer(q), "now").unwrap();
            let posted = gh
                .asked("POST")
                .into_iter()
                .find(|a| a.1.ends_with("/comments"))
                .unwrap();
            let text = posted.2.as_ref().unwrap()["body"].as_str().unwrap();
            assert!(text.contains("[redacted]"), "{field}");
            assert!(
                !text.contains(&hidden),
                "{field}: credential crossed GitHub boundary"
            );
        }
    }
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

#[test]
fn usage_accounts_for_engines_resumes_and_missing_results_without_text() {
    use std::fs::FileTimes;
    use std::time::{Duration, UNIX_EPOCH};
    let tmp = tempfile::tempdir().unwrap();
    let jobs = tmp.path().join("jobs");
    let job = jobs.join("example");
    fs::create_dir_all(&job).unwrap();
    fs::write(job.join("role"), "repair").unwrap();
    fs::write(job.join("usage-kind"), "inbox labels=codex opener=owner").unwrap();
    let put = |n: u64, engine: &str, at: u64, text: &str| {
        fs::write(job.join(format!("{n}.engine")), engine).unwrap();
        fs::write(job.join(format!("{n}.jsonl")), text).unwrap();
        fs::write(job.join(format!("{n}.started")), at.to_string()).unwrap();
        let f = fs::File::create(job.join(format!("{n}.slot"))).unwrap();
        f.set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(at)))
            .unwrap();
    };
    let result = r#"{"type":"result","session_id":"s","usage":{"input_tokens":10,"output_tokens":20,"cache_read_input_tokens":30,"cache_creation_input_tokens":40},"total_cost_usd":1.5,"result":"SECRET PROMPT"}"#;
    put(1, "claude", 100, result);
    put(2, "claude", 200, result);
    put(3, "codex", 250, r#"{"type":"turn.completed"}"#);
    put(
        4,
        "claude",
        300,
        r#"{"type":"assistant","message":{"id":"m","usage":{"input_tokens":7,"output_tokens":9},"content":"SECRET PROMPT"}}"#,
    );
    let other = jobs.join("manual");
    fs::create_dir_all(&other).unwrap();
    fs::write(other.join("starter"), "operator-session").unwrap();
    fs::write(other.join("1.engine"), "codex").unwrap();
    fs::write(other.join("1.jsonl"), "{}\n").unwrap();
    fs::write(other.join("1.started"), "275").unwrap();
    fs::File::create(other.join("1.slot"))
        .unwrap()
        .set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(275)))
        .unwrap();
    let state = tmp.path().join("state");
    fs::create_dir_all(state.join("usage-reset/meters")).unwrap();
    fs::write(state.join("usage-reset/meters/log"), concat!(
        "{\"engine\":\"claude\",\"read_at\":150,\"windows\":{\"five_hour\":{\"used\":100},\"seven_day\":{\"used\":60}}}\n",
        "{\"engine\":\"codex\",\"read_at\":250,\"windows\":{\"300m\":{\"used\":20},\"10080m\":{\"used\":40}}}\n",
        "{\"engine\":\"claude\",\"read_at\":10,\"windows\":{\"five_hour\":{\"used\":99}}}\n"
    )).unwrap();
    let mut cfg = config(&state);
    cfg.jobs = Some(jobs);
    let fake = Fake::default();
    let m = Machine {
        cfg: &cfg,
        root: "/".into(),
        now: 400,
        uid: 1,
        run: &fake,
    };
    let answer = m.answer(&Query::parse("usage @150").unwrap()).unwrap();
    assert!(answer.contains("claude: 2 turns"), "{answer}");
    assert!(answer.contains("codex: 2 turns"));
    assert!(answer.contains("role repair; inbox labels=codex opener=owner"));
    assert!(answer.contains("input=10 output=20 cache-read=30 cache-write=40 cost=$0.000000"));
    assert!(answer.contains(
        "input=7 output=0 cache-read=0 cache-write=0 known-cost=$0.000000; partial tokens (output counts completed runs only); additional cost unavailable"
    ));
    assert!(answer.contains("claude five_hour: 100% HIT 100%"));
    assert!(answer.contains("codex seven_day: 40%"));
    assert!(answer.contains("codex / no role; starter=operator-session: 1 turns"));
    assert!(!answer.contains("99%"));
    assert!(!answer.contains("SECRET"));
    let full = m.answer(&Query::parse("usage @0").unwrap()).unwrap();
    assert!(
        full.contains("input=10 output=20 cache-read=30 cache-write=40 cost=$1.500000"),
        "{full}"
    );
    assert!(fake.asked.borrow().is_empty());
    assert_eq!(
        Query::parse("usage").unwrap(),
        Query::Usage {
            since: crate::query::When::Ago(18000)
        }
    );
    assert!(Query::parse("usage -1h extra").is_err());
}

#[test]
fn usage_cannot_read_symlinked_metadata_transcripts_or_job_directories() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let jobs = root.join("jobs");
    let job = jobs.join("job");
    fs::create_dir_all(&job).unwrap();
    let secret = "ordinary confidential text";
    fs::write(root.join("secret"), secret).unwrap();
    fs::write(root.join("stream-secret"), "{\"type\":\"result\",\"session_id\":\"s\",\"usage\":{\"input_tokens\":9999},\"total_cost_usd\":9999}\n").unwrap();
    fs::write(job.join("1.started"), "200").unwrap();
    fs::File::create(job.join("1.slot"))
        .unwrap()
        .set_times(
            std::fs::FileTimes::new()
                .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(200)),
        )
        .unwrap();
    fs::write(job.join("1.jsonl"), "{}\n").unwrap();
    let mut cfg = config(&root.join("state"));
    cfg.jobs = Some(jobs.clone());
    let fake = Fake::default();
    let m = Machine {
        cfg: &cfg,
        root: "/".into(),
        now: 400,
        uid: 1,
        run: &fake,
    };
    for field in ["role", "usage-kind", "starter", "link", "1.engine"] {
        symlink(root.join("secret"), job.join(field)).unwrap();
        let got = m.answer(&Query::parse("usage @0").unwrap()).unwrap();
        assert!(!got.contains(secret), "{field}: {got}");
        fs::remove_file(job.join(field)).unwrap();
    }
    fs::remove_file(job.join("1.jsonl")).unwrap();
    symlink(root.join("stream-secret"), job.join("1.jsonl")).unwrap();
    let got = m.answer(&Query::parse("usage @0").unwrap()).unwrap();
    assert!(!got.contains("9999"));
    fs::remove_file(job.join("1.jsonl")).unwrap();
    fs::write(job.join("1.jsonl"), "{}\n").unwrap();
    fs::rename(&job, root.join("private-job")).unwrap();
    symlink(root.join("private-job"), &job).unwrap();
    let got = m.answer(&Query::parse("usage @0").unwrap()).unwrap();
    assert!(got.contains("claude: 0 turns"));
}

#[test]
fn usage_retains_baselines_outside_the_span_and_marks_a_later_unfinished_run() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let jobs = root.join("jobs");
    let job = jobs.join("job");
    fs::create_dir_all(&job).unwrap();
    let result = |input, cost| {
        serde_json::json!({"type":"result","session_id":"s","usage":{"input_tokens":input,"output_tokens":3},"total_cost_usd":cost}).to_string()
    };
    let put = |n, at, text: String| {
        fs::write(job.join(format!("{n}.started")), format!("{at}")).unwrap();
        fs::write(job.join(format!("{n}.jsonl")), text).unwrap();
    };
    put(1, 100, result(100, 1.5));
    put(
        2,
        200,
        format!(
            "{}\n{}\n",
            serde_json::json!({"type":"assistant","session_id":"s","message":{"id":"m","usage":{"input_tokens":7}}}),
            serde_json::json!({"type":"result","session_id":"s","subtype":"error_during_execution","usage":{"input_tokens":0},"total_cost_usd":0})
        ),
    );
    put(
        3,
        300,
        format!(
            "{}\n{}\n{}\n",
            result(80, 2.0),
            serde_json::json!({"type":"system","subtype":"init","session_id":"s"}),
            serde_json::json!({"type":"assistant","session_id":"s","message":{"id":"last","usage":{"input_tokens":9}}})
        ),
    );
    let mut cfg = config(&root.join("state"));
    cfg.jobs = Some(jobs);
    let fake = Fake::default();
    let m = Machine {
        cfg: &cfg,
        root: "/".into(),
        now: 400,
        uid: 1,
        run: &fake,
    };
    let got = m.answer(&Query::parse("usage @150").unwrap()).unwrap();
    assert!(
        got.contains("input=89 output=3 cache-read=0 cache-write=0 known-cost=$0.500000"),
        "{got}"
    );
    assert!(got.contains("cost may include unreported earlier runs"));
    assert!(got.contains("additional cost unavailable"));
    assert!(!got.contains("cost=$2.000000"));
}

#[test]
fn usage_first_start_survives_slot_recreation_and_unknown_history_is_explicit() {
    use std::fs::FileTimes;
    use std::time::{Duration, UNIX_EPOCH};
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let jobs = root.join("jobs");
    let job = jobs.join("job");
    fs::create_dir_all(&job).unwrap();
    fs::write(job.join("1.started"), "100").unwrap();
    fs::write(job.join("1.jsonl"), "{}\n").unwrap();
    fs::write(job.join("2.jsonl"), "{}\n").unwrap();
    fs::File::options()
        .write(true)
        .open(job.join("1.jsonl"))
        .unwrap()
        .set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(300)))
        .unwrap();
    fs::write(job.join("3.jsonl"), "").unwrap();
    let mut cfg = config(&root.join("state"));
    cfg.jobs = Some(jobs);
    let fake = Fake::default();
    let m = Machine {
        cfg: &cfg,
        root: "/".into(),
        now: 400,
        uid: 1,
        run: &fake,
    };
    for has_slot in [false, true] {
        if has_slot {
            fs::File::create(job.join("1.slot"))
                .unwrap()
                .set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(300)))
                .unwrap();
        }
        let got = m.answer(&Query::parse("usage @150").unwrap()).unwrap();
        assert!(got.contains("claude: 0 turns"), "{got}");
        assert!(got.contains("1 turns without recorded execution"));
        assert!(got.contains("1 historical turns with unknown first start"));
    }
    assert!(
        m.answer(&Query::parse("usage @0").unwrap())
            .unwrap()
            .contains("claude: 1 turns")
    );
}
