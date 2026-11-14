//! burrow-units: what's broken on the machine itself, as a source for the
//! stall watchdog (modules/watchdog/units.nix runs it). A unit that has
//! failed, and a program that dumped core, is an item in its watch file
//! from the moment it's seen, due a grace later: past that the watchdog
//! hands it to a Claude job like any other stall, which finds out why and
//! fixes the cause, then to the stuck issue, then to the owner. A unit that
//! runs again, or is reset, and a crash older than the window, leave the
//! file, and the stall clears with them.
//!
//! A pass starts when a unit fails or a program dumps core (its watcher
//! follows the journal for systemd's and systemd-coredump's word of it,
//! and asks its burrow-events handler), and on the hourly catch-up: no
//! timer of its own, and no unit's own settings touched.

use std::collections::BTreeMap;

use burrow_watchdog::watch::Item;

/// A unit in the failed state: its name, how it failed (systemd's
/// `Result`), and when it last changed state, in seconds since the epoch.
#[derive(Clone, Debug, PartialEq)]
pub struct Failed {
    pub unit: String,
    pub result: String,
    pub since: f64,
}

/// A core dump: the program, its pid, the signal that killed it, and when,
/// in seconds since the epoch.
#[derive(Clone, Debug, PartialEq)]
pub struct Core {
    pub exe: String,
    pub pid: u64,
    pub sig: u64,
    pub at: f64,
}

/// What a pass found, and when each program's crashes were last cleared
/// (`burrow-units clear EXE`, once its cause is fixed), by its path.
pub struct Found<'a> {
    pub failed: &'a [Failed],
    pub cores: &'a [Core],
    pub cleared: &'a BTreeMap<String, f64>,
}

/// The watch file's items for what was FOUND: one a failed unit
/// (`unit:NAME`) and one a program that crashed (`core:EXE`, its crashes
/// counted), leaving out what IGNORE's globs match (a unit's name, or a
/// program's path). Each is due GRACE after it was first seen: an item
/// LAST had (the source's last watch file) keeps its `since` while it's
/// still there, so a unit failing again on a timer, or a program crashing
/// again, is still the one stall, not a new one each time. A program's
/// crashes up to when they were cleared don't count, so it leaves once
/// fixed, and a crash after that is a stall of its own again.
pub fn items(
    found: &Found,
    ignore: &[String],
    grace: f64,
    last: &BTreeMap<String, Item>,
) -> BTreeMap<String, Item> {
    let ignored = |subject: &str| {
        ignore
            .iter()
            .any(|g| burrow_events::glob::matches(g, subject))
    };
    let since = |key: &str, phase: &str, at: f64, cleared: f64| {
        last.get(key)
            .filter(|i| i.phase == phase && i.since > cleared)
            .map_or(at, |i| i.since.min(at))
    };
    let cleared = |exe: &str| found.cleared.get(exe).copied().unwrap_or(f64::NEG_INFINITY);
    let mut out = BTreeMap::new();
    for f in found.failed.iter().filter(|f| !ignored(&f.unit)) {
        let key = format!("unit:{}", f.unit);
        let phase = format!("failed ({})", f.result);
        let since = since(&key, &phase, f.since, f64::NEG_INFINITY);
        out.insert(
            key,
            Item {
                what: format!("The unit {} failed", f.unit),
                phase,
                since,
                due: since + grace,
                ..Item::default()
            },
        );
    }
    let mut by_exe: BTreeMap<&str, Vec<&Core>> = BTreeMap::new();
    for c in found
        .cores
        .iter()
        .filter(|c| !ignored(&c.exe) && c.at > cleared(&c.exe))
    {
        by_exe.entry(&c.exe).or_default().push(c);
    }
    for (exe, mut cores) in by_exe {
        cores.sort_by(|a, b| a.at.total_cmp(&b.at));
        let (first, latest) = (cores[0], cores[cores.len() - 1]);
        let key = format!("core:{exe}");
        let phase = "dumped core".to_owned();
        let since = since(&key, &phase, first.at, cleared(exe));
        let times = match cores.len() {
            1 => String::new(),
            n => format!(" {n} times"),
        };
        out.insert(
            key,
            Item {
                what: format!(
                    "{exe} crashed{times} (last: signal {}, pid {})",
                    latest.sig, latest.pid
                ),
                phase,
                since,
                due: since + grace,
                ..Item::default()
            },
        );
    }
    out
}

/// The failed units in `systemctl list-units --state=failed --plain
/// --no-legend --full` TEXT: each line's first word.
pub fn failed_names(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.split_whitespace().next())
        .map(str::to_owned)
        .collect()
}

/// The units in `systemctl show -p Id,Result,StateChangeTimestamp
/// --timestamp=unix` TEXT, a block a unit. One whose time can't be read is
/// dated NOW: it's failed, whenever that was.
pub fn shown(text: &str, now: f64) -> Vec<Failed> {
    text.split("\n\n")
        .filter_map(|block| {
            let field = |name: &str| {
                block
                    .lines()
                    .find_map(|l| l.strip_prefix(name)?.strip_prefix('='))
                    .map(str::trim)
            };
            let unit = field("Id").filter(|u| !u.is_empty())?;
            let since = field("StateChangeTimestamp")
                .and_then(|t| t.strip_prefix('@'))
                .and_then(|t| t.parse::<f64>().ok())
                .unwrap_or(now);
            Some(Failed {
                unit: unit.to_owned(),
                result: field("Result").unwrap_or("unknown").to_owned(),
                since,
            })
        })
        .collect()
}

/// Crashes that aren't the machine's: what ran them sees them fail, and
/// owns finding out why.
#[derive(Clone, Debug, Default)]
pub struct Owned {
    /// The Nix build users' group: a crash in a build fails that build,
    /// for whoever asked for it (a job trying a package, CI, an update).
    pub build_gid: Option<u64>,
    /// Slices whose units run someone's own work (proj's jobs): a crash
    /// in one, a proof of concept or a test binary, is that job's to see.
    pub slices: Vec<String>,
}

impl Owned {
    /// Whether a process of GID, in CGROUP, ran someone's own work.
    fn owns(&self, gid: Option<u64>, cgroup: &str) -> bool {
        (gid.is_some() && gid == self.build_gid)
            || cgroup
                .split('/')
                .any(|part| self.slices.iter().any(|s| s == part))
    }
}

/// The machine's core dumps in TEXT, `journalctl -o json` of
/// systemd-coredump's entries, kernel dumps and submitted backtraces alike
/// (an object a line), leaving out what OWNED says ran someone's own work;
/// or why it can't be read: a dump read wrong could drop a crash that's
/// still there.
pub fn cores(text: &str, owned: &Owned) -> Result<Vec<Core>, String> {
    let mut out = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let d: serde_json::Value =
            serde_json::from_str(line).map_err(|e| format!("the journal's dumps: {e}"))?;
        let field = |name: &str| journal_field(&d[name]);
        let number = |name: &str| field(name).and_then(|v| v.parse::<u64>().ok());
        // Whose it was comes first: an owned dump is left out whatever
        // else it lacks.
        let cgroup = field("COREDUMP_CGROUP").unwrap_or_default();
        if owned.owns(number("COREDUMP_GID"), &cgroup) {
            continue;
        }
        let core = || {
            Some(Core {
                // As coredumpctl names it: a program whose path is gone
                // (its main thread exited first) by its command name.
                exe: field("COREDUMP_EXE").or_else(|| field("COREDUMP_COMM"))?,
                pid: number("COREDUMP_PID")?,
                sig: number("COREDUMP_SIGNAL").unwrap_or(0),
                // When it was logged, in microseconds: COREDUMP_TIMESTAMP
                // is whole seconds for the kernel's dumps, too coarse to
                // tell a crash from a clear in the same second.
                at: number("__REALTIME_TIMESTAMP")? as f64 / 1e6,
            })
        };
        out.push(core().ok_or_else(|| format!("the journal has a dump it can't read: {d}"))?);
    }
    Ok(out)
}

/// A journal field's value: a string, or the bytes it couldn't send as one.
fn journal_field(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Array(bytes) => {
            let bytes: Option<Vec<u8>> = bytes
                .iter()
                .map(|b| b.as_u64().and_then(|b| u8::try_from(b).ok()))
                .collect();
            Some(String::from_utf8_lossy(&bytes?).into_owned())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failed(unit: &str, since: f64) -> Failed {
        Failed {
            unit: unit.into(),
            result: "exit-code".into(),
            since,
        }
    }

    fn core(exe: &str, pid: u64, at: f64) -> Core {
        Core {
            exe: exe.into(),
            pid,
            sig: 11,
            at,
        }
    }

    #[test]
    fn each_failed_unit_and_crashed_program_is_a_stall_due_a_grace_on() {
        let failed = [
            failed("backup.service", 100.0),
            failed("noisy.service", 50.0),
        ];
        let cores = [
            core("/bin/worker", 7, 300.0),
            core("/bin/worker", 9, 200.0),
            core("/bin/other", 3, 250.0),
        ];
        let got = items(
            &Found {
                failed: &failed,
                cores: &cores,
                cleared: &BTreeMap::new(),
            },
            &["noisy.*".to_owned(), "/bin/oth*".to_owned()],
            600.0,
            &BTreeMap::new(),
        );
        assert_eq!(
            got.keys().collect::<Vec<_>>(),
            ["core:/bin/worker", "unit:backup.service"]
        );
        let unit = &got["unit:backup.service"];
        assert_eq!((unit.since, unit.due), (100.0, 700.0));
        assert_eq!(unit.phase, "failed (exit-code)");
        assert_eq!(unit.kick, None);
        let worker = &got["core:/bin/worker"];
        // From its first crash, and its last one named.
        assert_eq!((worker.since, worker.due), (200.0, 800.0));
        assert_eq!(
            worker.what,
            "/bin/worker crashed 2 times (last: signal 11, pid 7)"
        );
    }

    #[test]
    fn failing_again_is_the_same_stall_and_failing_another_way_a_new_one() {
        let first = items(
            &Found {
                failed: &[failed("backup.service", 100.0)],
                cores: &[],
                cleared: &BTreeMap::new(),
            },
            &[],
            600.0,
            &BTreeMap::new(),
        );
        // Its timer ran it again an hour on, and it failed again.
        let again = items(
            &Found {
                failed: &[failed("backup.service", 3700.0)],
                cores: &[],
                cleared: &BTreeMap::new(),
            },
            &[],
            600.0,
            &first,
        );
        assert_eq!(again["unit:backup.service"].since, 100.0);
        let other = Failed {
            result: "core-dump".into(),
            ..failed("backup.service", 3700.0)
        };
        let new = items(
            &Found {
                failed: &[other],
                cores: &[],
                cleared: &BTreeMap::new(),
            },
            &[],
            600.0,
            &first,
        );
        assert_eq!(new["unit:backup.service"].since, 3700.0);
        // Gone once it runs again: nothing left to watch.
        let fixed = items(
            &Found {
                failed: &[],
                cores: &[],
                cleared: &BTreeMap::new(),
            },
            &[],
            600.0,
            &first,
        );
        assert!(fixed.is_empty());
    }

    #[test]
    fn a_cleared_crash_leaves_and_a_later_one_is_a_stall_of_its_own() {
        let cores = [core("/bin/worker", 7, 200.0), core("/bin/worker", 9, 300.0)];
        fn pass(
            cleared: &BTreeMap<String, f64>,
            cores: &[Core],
            last: &BTreeMap<String, Item>,
        ) -> BTreeMap<String, Item> {
            items(
                &Found {
                    failed: &[],
                    cores,
                    cleared,
                },
                &[],
                600.0,
                last,
            )
        }
        let first = pass(&BTreeMap::new(), &cores, &BTreeMap::new());
        assert_eq!(first["core:/bin/worker"].since, 200.0);
        // Fixed and cleared at 400: both crashes are done with.
        let cleared = BTreeMap::from([("/bin/worker".to_owned(), 400.0)]);
        assert!(pass(&cleared, &cores, &first).is_empty());
        // It crashes again at 500, before a pass saw it go: a new stall,
        // due from the new crash, its earlier ones not counted.
        let again = [
            cores[0].clone(),
            cores[1].clone(),
            core("/bin/worker", 11, 500.0),
        ];
        let got = pass(&cleared, &again, &first);
        let worker = &got["core:/bin/worker"];
        assert_eq!((worker.since, worker.due), (500.0, 1100.0));
        assert_eq!(worker.what, "/bin/worker crashed (last: signal 11, pid 11)");
    }

    #[test]
    fn systemctl_and_the_journal_read() {
        let names = failed_names(
            "backup.service loaded failed failed Backup\nx@a\\x2db.service loaded failed failed X\n",
        );
        assert_eq!(names, ["backup.service", "x@a\\x2db.service"]);
        let units = shown(
            "Result=exit-code\nId=backup.service\nStateChangeTimestamp=@1793000000\n\nId=odd.service\nResult=timeout\nStateChangeTimestamp=\n",
            5.0,
        );
        assert_eq!(
            units,
            [
                Failed {
                    unit: "backup.service".into(),
                    result: "exit-code".into(),
                    since: 1_793_000_000.0
                },
                Failed {
                    unit: "odd.service".into(),
                    result: "timeout".into(),
                    since: 5.0
                },
            ]
        );
        let owned = Owned {
            build_gid: Some(30000),
            slices: vec!["claude-jobs.slice".into()],
        };
        let dumps = cores(
            concat!(
                // A kernel dump: its own time is whole seconds, when it was
                // logged isn't.
                r#"{"COREDUMP_EXE":"/nix/store/x/bin/worker","COREDUMP_PID":"42","COREDUMP_SIGNAL":"6","COREDUMP_TIMESTAMP":"1793000000000000","__REALTIME_TIMESTAMP":"1793000000123456","COREDUMP_GID":"1001","COREDUMP_CGROUP":"/system.slice/worker.service"}"#,
                "\n",
                // Not UTF-8, so the journal sends its bytes.
                r#"{"COREDUMP_EXE":[47,98,105,110,47,255],"COREDUMP_PID":"7","__REALTIME_TIMESTAMP":"1793000001000000"}"#,
                "\n",
                // Its path gone with its main thread: named as coredumpctl
                // names it, by its command.
                r#"{"COREDUMP_COMM":"threads","COREDUMP_PID":"8","COREDUMP_SIGNAL":"11","__REALTIME_TIMESTAMP":"1793000002000000"}"#,
                "\n",
                // An owned dump is left out, whatever else it lacks.
                r#"{"COREDUMP_GID":"30000","__REALTIME_TIMESTAMP":"1793000003000000"}"#,
                "\n",
            ),
            &owned,
        )
        .unwrap();
        assert_eq!(
            dumps,
            [
                Core {
                    exe: "/nix/store/x/bin/worker".into(),
                    pid: 42,
                    sig: 6,
                    at: 1_793_000_000.123456,
                },
                Core {
                    exe: "/bin/\u{fffd}".into(),
                    pid: 7,
                    sig: 0,
                    at: 1_793_000_001.0,
                },
                Core {
                    exe: "threads".into(),
                    pid: 8,
                    sig: 11,
                    at: 1_793_000_002.0,
                },
            ]
        );
        // Anything else than dumps it can read is an error, never none:
        // that would clear crashes still there. No entries is none.
        assert_eq!(cores("", &owned), Ok(Vec::new()));
        assert!(cores("No coredumps found.", &owned).is_err());
        assert!(cores(r#"{"COREDUMP_PID":"2","__REALTIME_TIMESTAMP":"1"}"#, &owned).is_err());
    }

    #[test]
    fn a_crash_in_the_same_second_as_its_clear_is_back() {
        // Cleared at 100.2; the kernel's dump of a crash at 100.8 says 100.
        let cleared = BTreeMap::from([("/bin/worker".to_owned(), 100.2)]);
        let dump = |logged: &str| {
            format!(
                r#"{{"COREDUMP_EXE":"/bin/worker","COREDUMP_PID":"1","COREDUMP_TIMESTAMP":"100000000","__REALTIME_TIMESTAMP":"{logged}"}}"#
            )
        };
        let pass = |logged: &str| {
            let cores = cores(&dump(logged), &Owned::default()).unwrap();
            items(
                &Found {
                    failed: &[],
                    cores: &cores,
                    cleared: &cleared,
                },
                &[],
                600.0,
                &BTreeMap::new(),
            )
        };
        assert!(pass("100100000").is_empty(), "logged before the clear");
        assert!(pass("100800000").contains_key("core:/bin/worker"));
    }

    #[test]
    fn a_crash_in_a_build_or_a_job_is_theirs_not_the_machines() {
        let owned = Owned {
            build_gid: Some(30000),
            slices: vec!["claude-jobs.slice".into()],
        };
        let dump = |exe: &str, gid: u64, cgroup: &str| {
            format!(
                r#"{{"COREDUMP_EXE":"{exe}","COREDUMP_PID":"1","__REALTIME_TIMESTAMP":"1793000000000000","COREDUMP_GID":"{gid}","COREDUMP_CGROUP":"{cgroup}"}}"#
            )
        };
        let text = [
            // A package's own version check, in a build.
            dump("/nix/store/b/bin/.claude-wrapped", 30000, "/builds.slice/nix-daemon.service"),
            // A job's proof of concept.
            dump(
                "/nix/store/x/bin/xsltproc",
                1001,
                "/user.slice/user-1001.slice/user@1001.service/claude.slice/claude-jobs.slice/proj-remi\\\\x2d1.service",
            ),
            // The daemon itself, and the same user outside a job: the machine's.
            dump("/nix/store/n/bin/nix-daemon", 0, "/builds.slice/nix-daemon.service"),
            dump(
                "/nix/store/c/bin/claude",
                1001,
                "/user.slice/user-1001.slice/user@1001.service/claude.slice/claude-rc.service",
            ),
        ]
        .join("\n");
        let exes: Vec<String> = cores(&text, &owned)
            .unwrap()
            .into_iter()
            .map(|c| c.exe)
            .collect();
        assert_eq!(
            exes,
            ["/nix/store/n/bin/nix-daemon", "/nix/store/c/bin/claude"]
        );
        // Owning nothing, every crash is the machine's.
        assert_eq!(cores(&text, &Owned::default()).unwrap().len(), 4);
    }
}
