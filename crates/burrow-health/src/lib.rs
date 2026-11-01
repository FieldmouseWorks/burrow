//! HOST-health: is the machine at least as healthy as it was a moment ago?
//!
//!   HOST-health                         run the checks now and print them
//!   HOST-health snapshot FILE           record which checks pass, and which
//!                                       units have failed
//!   HOST-health compare FILE [SECONDS]  after a change: wait up to SECONDS
//!                                       (default 180) for everything that
//!                                       passed in FILE to pass again, and for
//!                                       no unit to have newly failed; exit 1
//!                                       and say what regressed if it doesn't
//!
//! The checks are the executables in the config's `dir` (/etc/burrow/health.d,
//! one small script each, from burrow.updates.health), so every module can
//! add its own. Comparing against a snapshot means something that was already
//! broken never blocks an update, and something an update breaks always does.
//!
//! A report has a line for each: `check:NAME ok` or `check:NAME fail` for
//! each check, in name order, then `unit:NAME fail` for each failed system
//! unit and `user-unit:NAME fail` for each of the user's. HOST-heartbeat
//! reads it too.

use std::ffi::OsString;
use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, WaitId, WaitIdOptions};
use serde::Deserialize;

/// What modules/updates.nix tells HOST-health, as JSON.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Config {
    /// HOST-health, as its usage names it.
    pub name: String,
    /// Where the checks are.
    pub dir: PathBuf,
    /// The user whose own units count too (burrow.user).
    pub user: String,
    pub systemctl: PathBuf,
    /// Seconds a check may run before it fails.
    pub check_timeout: u64,
    /// Seconds a compare waits before each look, to let restarts and
    /// reconnects settle.
    pub settle: u64,
}

impl Config {
    pub fn read(path: &Path) -> Result<Config, String> {
        let text = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_slice(&text).map_err(|e| format!("{}: {e}", path.display()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Ok,
    Fail,
}

/// One line of a report: what it's about (`check:NAME`, `unit:NAME` or
/// `user-unit:NAME`) and how that is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub key: String,
    pub state: State,
}

const CHECK: &str = "check:";
const UNITS: [&str; 2] = ["unit:", "user-unit:"];

/// A report as HOST-health prints it and a snapshot keeps it.
pub fn render(report: &[Line]) -> String {
    report
        .iter()
        .map(|l| {
            let state = match l.state {
                State::Ok => "ok",
                State::Fail => "fail",
            };
            format!("{} {state}\n", l.key)
        })
        .collect()
}

/// A report read back: each line is KEY and its state after the last space.
/// A line that isn't one is no line.
pub fn parse(text: &str) -> Vec<Line> {
    text.lines()
        .filter_map(|line| {
            let (key, state) = line.rsplit_once(' ')?;
            let state = match state {
                "ok" => State::Ok,
                "fail" => State::Fail,
                _ => return None,
            };
            Some(Line {
                key: key.to_owned(),
                state,
            })
        })
        .collect()
}

/// What's worse in `now` than in `before`, a line each: a check that passed
/// and fails now, a check that's new and fails, and a unit that has failed
/// since. Names are compared as they are, whatever characters they hold:
/// a unit's name has systemd's `\x2d` escapes in it as often as not.
pub fn regressions(before: &[Line], now: &[Line]) -> Vec<String> {
    let has = |lines: &[Line], key: &str| lines.iter().any(|l| l.key == key);
    let failing = |key: &str| now.iter().any(|l| l.key == key && l.state == State::Fail);
    let mut bad = Vec::new();
    for b in before {
        if b.key.starts_with(CHECK) && b.state == State::Ok && failing(&b.key) {
            bad.push(format!("{} is failing", b.key));
        }
    }
    for n in now {
        if n.key.starts_with(CHECK) && n.state == State::Fail && !has(before, &n.key) {
            bad.push(format!("{} (new) is failing", n.key));
        }
    }
    for n in now {
        let unit = UNITS.iter().find_map(|p| n.key.strip_prefix(p));
        if let Some(unit) = unit
            && !has(before, &n.key)
        {
            bad.push(format!("{unit} failed"));
        }
    }
    bad
}

/// The report now: every check, then the system's failed units and the
/// user's.
pub fn report(cfg: &Config) -> Vec<Line> {
    let mut lines = checks(&cfg.dir, Duration::from_secs(cfg.check_timeout));
    lines.extend(failed_units(cfg, false));
    lines.extend(failed_units(cfg, true));
    lines
}

/// Every executable in `dir` but its dotfiles, in name order, as a check
/// that passes or fails.
fn checks(dir: &Path, within: Duration) -> Vec<Line> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<OsString> = entries
        .filter_map(|e| e.ok().map(|e| e.file_name()))
        .filter(|n| !n.as_encoded_bytes().starts_with(b"."))
        .collect();
    names.sort();
    names
        .iter()
        .map(|n| dir.join(n))
        .filter(|path| rustix::fs::access(path, rustix::fs::Access::EXEC_OK).is_ok())
        .map(|path| Line {
            key: format!(
                "{CHECK}{}",
                path.file_name().unwrap_or_default().to_string_lossy()
            ),
            state: if passes(&path, within) {
                State::Ok
            } else {
                State::Fail
            },
        })
        .collect()
}

/// systemctl's failed units, the system's or the user's. Whatever systemctl
/// says is taken, even when it then fails. The system's complaints are on
/// stderr; the user's manager may simply not be running, so its aren't.
fn failed_units(cfg: &Config, users: bool) -> Vec<Line> {
    let mut run = Command::new(&cfg.systemctl);
    if users {
        run.args(["--user", "-M"]).arg(format!("{}@", cfg.user));
    }
    run.args(["list-units", "--failed", "--plain", "--no-legend", "--all"])
        .stdin(Stdio::null())
        .stderr(if users {
            Stdio::null()
        } else {
            Stdio::inherit()
        });
    let out = match run.output() {
        Ok(out) => out,
        Err(e) => {
            if !users {
                eprintln!("{}: {}: {e}", cfg.name, cfg.systemctl.display());
            }
            return Vec::new();
        }
    };
    let prefix = UNITS[usize::from(users)];
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .map(|unit| Line {
            key: format!("{prefix}{unit}"),
            state: State::Fail,
        })
        .collect()
}

/// How long a check that ran out of time has after its TERM before
/// whatever is left of it is killed.
const GRACE: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(10);

/// Whether the check at `path` exits 0 within `within`. It runs with
/// nothing on stdin and its output thrown away, in a process group of its
/// own. One that runs out of time fails, and its whole group is stopped:
/// TERM, as timeout(1) sends it, then after GRACE a KILL for whatever is
/// left, so a check that ignores its TERM doesn't hold HOST-health up.
pub fn passes(path: &Path, within: Duration) -> bool {
    let Ok(mut child) = Command::new(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
    else {
        return false;
    };
    let finished = exits(&child, within);
    if !finished {
        // Until it's waited for, the check's process ID, and so its group's,
        // can't be anyone else's.
        let group = Pid::from_child(&child);
        let _ = rustix::process::kill_process_group(group, Signal::TERM);
        exits(&child, GRACE);
        let _ = rustix::process::kill_process_group(group, Signal::KILL);
    }
    let status = child.wait();
    finished && status.is_ok_and(|s| s.success())
}

/// Whether `child` exits within `within`, leaving it to be waited for.
fn exits(child: &Child, within: Duration) -> bool {
    let pid = Pid::from_child(child);
    let deadline = Instant::now().checked_add(within);
    loop {
        match rustix::process::waitid(
            WaitId::Pid(pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        ) {
            Ok(None) => {}
            // It exited, or there's nothing left to wait for.
            Ok(Some(_)) | Err(_) => return true,
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            return false;
        }
        sleep(POLL);
    }
}

/// After a change: `look`, after `settle`, until nothing has regressed
/// since `before`, for up to `within`. What regressed at the last look if
/// it never clears.
pub fn compare(
    before: &[Line],
    within: Duration,
    settle: Duration,
    mut look: impl FnMut() -> Vec<Line>,
) -> Result<(), Vec<String>> {
    let deadline = Instant::now().checked_add(within);
    loop {
        sleep(settle);
        let bad = regressions(before, &look());
        if bad.is_empty() {
            return Ok(());
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            return Err(bad);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<Line> {
        parse(text)
    }

    #[test]
    fn a_report_reads_back_as_it_was_written() {
        let report = vec![
            Line {
                key: "check:dns".into(),
                state: State::Ok,
            },
            Line {
                key: "check:the web site".into(),
                state: State::Fail,
            },
            Line {
                key: r"unit:systemd-fsck@dev-disk-by\x2duuid-1.service".into(),
                state: State::Fail,
            },
        ];
        let text = render(&report);
        assert_eq!(
            text,
            "check:dns ok\ncheck:the web site fail\nunit:systemd-fsck@dev-disk-by\\x2duuid-1.service fail\n"
        );
        assert_eq!(parse(&text), report);
        // What isn't a line is left out.
        assert_eq!(
            parse("check:dns maybe\nnothing\n\ncheck:a ok"),
            lines("check:a ok")
        );
    }

    #[test]
    fn what_regressed_and_what_didnt() {
        let before = lines(
            "check:a ok\ncheck:b ok\ncheck:c fail\ncheck:gone ok\nunit:old.service fail\nuser-unit:old.service fail\n",
        );
        // Healthy as before, a check gone, a broken one still broken, and
        // the old failures still there.
        let now = lines(
            "check:a ok\ncheck:b ok\ncheck:c fail\nunit:old.service fail\nuser-unit:old.service fail\n",
        );
        assert_eq!(regressions(&before, &now), Vec::<String>::new());
        let now = lines(
            "check:a fail\ncheck:b ok\ncheck:c fail\ncheck:d fail\ncheck:e ok\nunit:old.service fail\nunit:new.service fail\nuser-unit:new.service fail\n",
        );
        assert_eq!(
            regressions(&before, &now),
            [
                "check:a is failing",
                "check:d (new) is failing",
                "new.service failed",
                "new.service failed",
            ]
        );
        // A system unit's failure isn't the user's, nor the other way round.
        let before = lines("unit:x.service fail\n");
        let now = lines("user-unit:x.service fail\n");
        assert_eq!(regressions(&before, &now), ["x.service failed"]);
    }

    #[test]
    fn names_are_compared_as_they_are() {
        // A unit that had already failed, its name full of escapes: the
        // same unit, not a new failure.
        let unit = r"unit:systemd-fsck@dev-disk-by\x2duuid-0a1b.service fail";
        assert_eq!(
            regressions(&lines(unit), &lines(unit)),
            Vec::<String>::new()
        );
        // A name that would match others as a pattern matches only itself:
        // a.c still passes though abc fails, and a.service's failure is new
        // though aXservice's isn't.
        let before = lines("check:a.c ok\nunit:aXservice fail\n");
        let now = lines("check:a.c ok\ncheck:abc fail\nunit:aXservice fail\nunit:a.service fail\n");
        assert_eq!(
            regressions(&before, &now),
            ["check:abc (new) is failing", "a.service failed"]
        );
        // One name that starts another isn't it, either way round.
        let before = lines("check:web ok\n");
        let now = lines("check:web ok\ncheck:web site fail\n");
        assert_eq!(
            regressions(&before, &now),
            ["check:web site (new) is failing"]
        );
        let before = lines("check:web site ok\nunit:a.service.old fail\n");
        let now = lines("check:web fail\nunit:a.service fail\n");
        assert_eq!(
            regressions(&before, &now),
            ["check:web (new) is failing", "a.service failed"]
        );
    }

    #[test]
    fn a_compare_looks_until_it_clears_or_time_is_up() {
        let before = lines("check:a ok\n");
        let mut looks = 0;
        let cleared = compare(&before, Duration::from_secs(60), Duration::ZERO, || {
            looks += 1;
            lines(if looks < 3 {
                "check:a fail"
            } else {
                "check:a ok"
            })
        });
        assert_eq!((cleared, looks), (Ok(()), 3));
        // With no time, one look.
        let mut looks = 0;
        let stuck = compare(&before, Duration::ZERO, Duration::ZERO, || {
            looks += 1;
            lines("check:a fail")
        });
        assert_eq!(
            (stuck, looks),
            (Err(vec!["check:a is failing".to_owned()]), 1)
        );
        // A time too long to add up is no deadline, not a panic.
        let mut looks = 0;
        let cleared = compare(&before, Duration::MAX, Duration::ZERO, || {
            looks += 1;
            lines(if looks < 2 {
                "check:a fail"
            } else {
                "check:a ok"
            })
        });
        assert_eq!(cleared, Ok(()));
    }
}
