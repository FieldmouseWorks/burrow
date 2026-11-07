//! burrow-units: what's broken on the machine itself, as a source for the
//! stall watchdog (modules/watchdog/units.nix runs it). A unit that has
//! failed, and a program that dumped core, is an item in its watch file
//! from the moment it's seen, due a grace later: past that the watchdog
//! hands it to a Claude job like any other stall, which finds out why and
//! fixes the cause, then to the stuck issue, then to the owner. A unit that
//! runs again, or is reset, and a crash older than the window, leave the
//! file, and the stall clears with them.
//!
//! A pass starts when a unit fails or a program dumps core (systemd's
//! OnFailure and OnSuccess, kicking its burrow-events handler), and on the
//! hourly catch-up: no timer of its own.

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

/// What a pass found.
pub struct Found<'a> {
    pub failed: &'a [Failed],
    pub cores: &'a [Core],
}

/// The watch file's items for what was FOUND: one a failed unit
/// (`unit:NAME`) and one a program that crashed (`core:EXE`, its crashes
/// counted), leaving out what IGNORE's globs match (a unit's name, or a
/// program's path). Each is due GRACE after it was first seen: an item
/// LAST had (the source's last watch file) keeps its `since` while it's
/// still there, so a unit failing again on a timer, or a program crashing
/// again, is still the one stall, not a new one each time.
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
    let since = |key: &str, phase: &str, at: f64| {
        last.get(key)
            .filter(|i| i.phase == phase)
            .map_or(at, |i| i.since.min(at))
    };
    let mut out = BTreeMap::new();
    for f in found.failed.iter().filter(|f| !ignored(&f.unit)) {
        let key = format!("unit:{}", f.unit);
        let phase = format!("failed ({})", f.result);
        let since = since(&key, &phase, f.since);
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
    for c in found.cores.iter().filter(|c| !ignored(&c.exe)) {
        by_exe.entry(&c.exe).or_default().push(c);
    }
    for (exe, mut cores) in by_exe {
        cores.sort_by(|a, b| a.at.total_cmp(&b.at));
        let (first, latest) = (cores[0], cores[cores.len() - 1]);
        let key = format!("core:{exe}");
        let phase = "dumped core".to_owned();
        let since = since(&key, &phase, first.at);
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

/// The core dumps in `coredumpctl list --json=short` TEXT. Nothing, or a
/// dump it can't read, is none.
pub fn cores(text: &str) -> Vec<Core> {
    let Ok(serde_json::Value::Array(dumps)) = serde_json::from_str(text) else {
        return Vec::new();
    };
    dumps
        .iter()
        .filter_map(|d| {
            Some(Core {
                exe: d["exe"].as_str()?.to_owned(),
                pid: d["pid"].as_u64()?,
                sig: d["sig"].as_u64().unwrap_or(0),
                // In microseconds.
                at: d["time"].as_f64()? / 1e6,
            })
        })
        .collect()
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
            },
            &[],
            600.0,
            &first,
        );
        assert!(fixed.is_empty());
    }

    #[test]
    fn systemctl_and_coredumpctl_read() {
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
        let dumps = cores(
            r#"[{"time":1793000000123456,"pid":42,"uid":1001,"gid":1001,"sig":6,"corefile":"present","exe":"/nix/store/x/bin/worker","size":1}]"#,
        );
        assert_eq!(
            dumps,
            [Core {
                exe: "/nix/store/x/bin/worker".into(),
                pid: 42,
                sig: 6,
                at: 1_793_000_000.123456,
            }]
        );
        assert!(cores("No coredumps found.").is_empty());
    }
}
