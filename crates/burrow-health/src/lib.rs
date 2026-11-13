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
//! A check can explain others (`explains`, from burrow.updates.healthExplains):
//! while it fails, what can't pass without it (the operator's check and the
//! Remote Control units while Claude Code isn't logged in) isn't counted as
//! the change's doing. The check itself still is, as any other, so a change
//! that breaks the login is still a regression, and a login already gone
//! before the change doesn't revert it. A compare that passes says what it
//! didn't count, after `healthy`.
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

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, WaitId, WaitIdOptions};
use serde::Deserialize;

mod diagnostic;
use diagnostic::Diagnostic;

/// What modules/updates.nix tells HOST-health, as JSON.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Config {
    /// HOST-health, as its usage names it.
    pub name: String,
    /// Where the checks are.
    pub dir: PathBuf,
    /// Checks that must pass even if they failed before a switch.
    #[serde(default)]
    pub required: Vec<String>,
    /// By check: the report keys (`check:NAME`, `unit:NAME`,
    /// `user-unit:NAME`, where `*` stands for any run of characters) that
    /// can't pass while it fails.
    #[serde(default)]
    pub explains: BTreeMap<String, Vec<String>>,
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

impl Line {
    /// The check's name, on a check's line.
    pub fn check(&self) -> Option<&str> {
        self.key.strip_prefix(CHECK)
    }

    /// The unit's name, on a failed unit's line, the system's or the user's.
    pub fn unit(&self) -> Option<&str> {
        UNITS.iter().find_map(|p| self.key.strip_prefix(p))
    }
}

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

/// A report read back, for a compare and for the heartbeat: each line is
/// KEY and its state after the last space. A line that isn't one means the
/// report isn't what `render` wrote, so none of it is taken.
pub fn parse(text: &str) -> Result<Vec<Line>, String> {
    text.lines()
        .enumerate()
        .map(|(n, line)| {
            let state = match line.rsplit_once(' ') {
                Some((key, "ok")) => Some((key, State::Ok)),
                Some((key, "fail")) => Some((key, State::Fail)),
                _ => None,
            };
            match state {
                Some((key, state)) if !key.is_empty() => Ok(Line {
                    key: key.to_owned(),
                    state,
                }),
                _ => Err(format!(
                    "line {} isn't `KEY ok` or `KEY fail`: {line:?}",
                    n + 1
                )),
            }
        })
        .collect()
}

/// What's worse in `now` than in `before`, by key and a line each: a check
/// that passed and fails now, a check that's new and fails, and a unit that
/// has failed since. Names are compared as they are, whatever characters
/// they hold: a unit's name has systemd's `\x2d` escapes in it as often as
/// not.
fn regressed(before: &[Line], now: &[Line]) -> Vec<(String, String)> {
    let has = |lines: &[Line], key: &str| lines.iter().any(|l| l.key == key);
    let failing = |key: &str| now.iter().any(|l| l.key == key && l.state == State::Fail);
    let mut bad = Vec::new();
    for b in before {
        if b.check().is_some() && b.state == State::Ok && failing(&b.key) {
            bad.push((b.key.clone(), format!("{} is failing", b.key)));
        }
    }
    for n in now {
        if n.check().is_some() && n.state == State::Fail && !has(before, &n.key) {
            bad.push((n.key.clone(), format!("{} (new) is failing", n.key)));
        }
    }
    for n in now {
        if let Some(unit) = n.unit()
            && !has(before, &n.key)
        {
            bad.push((n.key.clone(), format!("{unit} failed")));
        }
    }
    bad
}

/// What regressed, split in two: what counts against the change, and what
/// a check failing in `now` explains (`explains`), said with its cause.
pub fn judge(
    before: &[Line],
    now: &[Line],
    explains: &BTreeMap<String, Vec<String>>,
) -> (Vec<String>, Vec<String>) {
    let failing: Vec<&str> = now
        .iter()
        .filter(|l| l.state == State::Fail)
        .filter_map(Line::check)
        .collect();
    let mut bad = Vec::new();
    let mut excused = Vec::new();
    for (key, said) in regressed(before, now) {
        // A check never explains itself away.
        let cause = failing.iter().find(|cause| {
            key != format!("{CHECK}{cause}")
                && explains
                    .get(**cause)
                    .is_some_and(|keys| keys.iter().any(|p| glob(p, &key)))
        });
        match cause {
            Some(cause) => excused.push(format!(
                "{said}, but not counted: check:{cause} is failing, and it can't pass without that"
            )),
            None => bad.push(said),
        }
    }
    (bad, excused)
}

/// Whether `key` is `pattern`, where each `*` in it stands for any run of
/// characters, none included, and everything else only for itself.
fn glob(pattern: &str, key: &str) -> bool {
    let mut parts = pattern.split('*');
    let first = parts.next().unwrap_or_default();
    let Some(mut rest) = key.strip_prefix(first) else {
        return false;
    };
    let parts: Vec<&str> = parts.collect();
    let Some((last, middle)) = parts.split_last() else {
        return rest.is_empty();
    };
    for part in middle {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    rest.len() >= last.len() && rest.ends_with(last)
}

/// The report now: every check, then the system's failed units and the
/// user's.
pub fn report(cfg: &Config) -> Vec<Line> {
    report_details(cfg).0
}

/// The report and bounded stderr from failed checks, run once per look.
pub fn report_details(cfg: &Config) -> (Vec<Line>, Vec<(String, String)>) {
    let (mut lines, mut details) = checks(&cfg.dir, Duration::from_secs(cfg.check_timeout));
    for name in &cfg.required {
        if !lines.iter().any(|l| l.check() == Some(name)) {
            let key = format!("{CHECK}{name}");
            lines.push(Line {
                key: key.clone(),
                state: State::Fail,
            });
            details.push((key, "required check is missing".into()));
        }
    }
    lines.extend(failed_units(cfg, false));
    lines.extend(failed_units(cfg, true));
    (lines, details)
}

/// Every executable in `dir` but its dotfiles, in name order, as a check
/// that passes or fails.
fn checks(dir: &Path, within: Duration) -> (Vec<Line>, Vec<(String, String)>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return (Vec::new(), Vec::new());
    };
    let mut names: Vec<OsString> = entries
        .filter_map(|e| e.ok().map(|e| e.file_name()))
        .filter(|n| !n.as_encoded_bytes().starts_with(b"."))
        .collect();
    names.sort();
    let mut details = Vec::new();
    let lines = names
        .iter()
        .map(|n| dir.join(n))
        .filter(|path| rustix::fs::access(path, rustix::fs::Access::EXEC_OK).is_ok())
        .map(|path| {
            let key = format!(
                "{CHECK}{}",
                path.file_name().unwrap_or_default().to_string_lossy()
            );
            let (passed, text) = run_check(&path, within);
            if !passed && !text.is_empty() {
                details.push((key.clone(), text));
            }
            Line {
                key,
                state: if passed { State::Ok } else { State::Fail },
            }
        })
        .collect();
    (lines, details)
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
/// nothing on stdin and captures bounded diagnostics, in a process group of its
/// own. One that runs out of time fails, and its whole group is stopped:
/// TERM, as timeout(1) sends it, then after GRACE a KILL for whatever is
/// left, so a check that ignores its TERM doesn't hold HOST-health up.
pub fn passes(path: &Path, within: Duration) -> bool {
    run_check(path, within).0
}

fn run_check(path: &Path, within: Duration) -> (bool, String) {
    let Ok(mut child) = Command::new(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
    else {
        return (false, String::new());
    };
    let mut diagnostic = match Diagnostic::new(child.stderr.take().unwrap()) {
        Ok(diagnostic) => diagnostic,
        Err(_) => {
            let _ = rustix::process::kill_process_group(Pid::from_child(&child), Signal::KILL);
            let _ = child.wait();
            return (false, "could not capture check stderr".into());
        }
    };
    let finished = exits(&child, within, &mut diagnostic);
    if !finished {
        // Until it's waited for, the check's process ID, and so its group's,
        // can't be anyone else's.
        let group = Pid::from_child(&child);
        let _ = rustix::process::kill_process_group(group, Signal::TERM);
        exits(&child, GRACE, &mut diagnostic);
        let _ = rustix::process::kill_process_group(group, Signal::KILL);
    }
    let status = child.wait();
    diagnostic.drain();
    (
        finished && status.is_ok_and(|s| s.success()),
        diagnostic.text(),
    )
}

/// Whether `child` exits within `within`, leaving it to be waited for.
fn exits(child: &Child, within: Duration, diagnostic: &mut Diagnostic) -> bool {
    let pid = Pid::from_child(child);
    let deadline = Instant::now().checked_add(within);
    loop {
        diagnostic.drain();
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

/// Required checks compare against success even when the snapshot failed.
pub fn require(before: &mut Vec<Line>, required: &[String]) {
    for name in required {
        let key = format!("{CHECK}{name}");
        if let Some(line) = before.iter_mut().find(|l| l.key == key) {
            line.state = State::Ok;
        } else {
            before.push(Line {
                key,
                state: State::Ok,
            });
        }
    }
}

/// After a change: `look`, after `settle`, until nothing has regressed
/// since `before` but what `explains` excuses, for up to `within`. What it
/// excused at the look that cleared, or what regressed at the last look if
/// it never clears.
pub fn compare(
    before: &[Line],
    explains: &BTreeMap<String, Vec<String>>,
    within: Duration,
    settle: Duration,
    mut look: impl FnMut() -> Vec<Line>,
) -> Result<Vec<String>, Vec<String>> {
    let deadline = Instant::now().checked_add(within);
    loop {
        sleep(settle);
        let (bad, excused) = judge(before, &look(), explains);
        if bad.is_empty() {
            return Ok(excused);
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
        parse(text).unwrap()
    }

    /// What regressed, nothing explained.
    fn regressions(before: &[Line], now: &[Line]) -> Vec<String> {
        judge(before, now, &BTreeMap::new()).0
    }

    #[test]
    fn noisy_checks_are_drained_with_bounded_capture_in_every_outcome() {
        let dir = tempfile::tempdir().unwrap();
        for (name, end, within, expected) in [
            ("success", "exit 0", Duration::from_secs(10), true),
            ("failure", "exit 1", Duration::from_secs(10), false),
            (
                "timeout",
                "while :; do printf '%s' \"$chunk\"; printf '%s' \"$chunk\" >&2; done",
                Duration::from_millis(200),
                false,
            ),
        ] {
            // A regular-file capture exceeds the child's file-size limit and
            // fails the success case. A bounded pipe drains without that disk use.
            let path = dir.path().join(name);
            burrow_test::executable(
                &path,
                format!(
                    "#!/bin/sh\nchunk=xxxxxxxx; for step in 1 2 3 4 5 6 7 8 9 10; do chunk=$chunk$chunk; done\nulimit -c 0; ulimit -f 1\ni=0; while [ \"$i\" -lt 256 ]; do printf '%s' \"$chunk\"; printf '%s' \"$chunk\" >&2; i=$((i+1)); done\n{end}\n"
                ),
            );
            let (passed, said) = run_check(&path, within);
            assert_eq!(passed, expected, "{name}");
            assert_eq!(said.len(), 4096, "{name}");
        }
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
        assert_eq!(parse(&text), Ok(report));
        assert_eq!(parse(""), Ok(vec![]));
        // A report with a line that isn't one isn't taken at all.
        for (text, bad) in [
            (
                "check:a ok\ncheck:dns maybe",
                "line 2 isn't `KEY ok` or `KEY fail`: \"check:dns maybe\"",
            ),
            (
                "nothing\ncheck:a ok",
                "line 1 isn't `KEY ok` or `KEY fail`: \"nothing\"",
            ),
            (
                "check:a ok\n\ncheck:b ok",
                "line 2 isn't `KEY ok` or `KEY fail`: \"\"",
            ),
            (" ok", "line 1 isn't `KEY ok` or `KEY fail`: \" ok\""),
            (
                "check:a OK",
                "line 1 isn't `KEY ok` or `KEY fail`: \"check:a OK\"",
            ),
            (
                "check:a ok ",
                "line 1 isn't `KEY ok` or `KEY fail`: \"check:a ok \"",
            ),
        ] {
            assert_eq!(parse(text), Err(bad.to_owned()), "{text:?}");
        }
    }

    #[test]
    fn a_line_says_what_its_about() {
        let all =
            lines("check:dns ok\nunit:a.service fail\nuser-unit:b.service fail\nother:c fail");
        let about: Vec<_> = all.iter().map(|l| (l.check(), l.unit())).collect();
        assert_eq!(
            about,
            [
                (Some("dns"), None),
                (None, Some("a.service")),
                (None, Some("b.service")),
                (None, None)
            ]
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
        let cleared = compare(
            &before,
            &BTreeMap::new(),
            Duration::from_secs(60),
            Duration::ZERO,
            || {
                looks += 1;
                lines(if looks < 3 {
                    "check:a fail"
                } else {
                    "check:a ok"
                })
            },
        );
        assert_eq!((cleared, looks), (Ok(vec![]), 3));
        // With no time, one look.
        let mut looks = 0;
        let stuck = compare(
            &before,
            &BTreeMap::new(),
            Duration::ZERO,
            Duration::ZERO,
            || {
                looks += 1;
                lines("check:a fail")
            },
        );
        assert_eq!(
            (stuck, looks),
            (Err(vec!["check:a is failing".to_owned()]), 1)
        );
        // A time too long to add up is no deadline, not a panic.
        let mut looks = 0;
        let cleared = compare(
            &before,
            &BTreeMap::new(),
            Duration::MAX,
            Duration::ZERO,
            || {
                looks += 1;
                lines(if looks < 2 {
                    "check:a fail"
                } else {
                    "check:a ok"
                })
            },
        );
        assert_eq!(cleared, Ok(vec![]));
    }

    #[test]
    fn a_login_gone_before_the_switch_explains_what_needs_it() {
        // The incident: Claude Code's login had expired before the switch,
        // the running servers kept going, and the switch restarted them into
        // a login that wasn't there.
        let explains = BTreeMap::from([(
            "claude-login".to_owned(),
            vec![
                "check:operator".to_owned(),
                "user-unit:claude-rc@*.service".to_owned(),
            ],
        )]);
        let before = lines("check:claude-login fail\ncheck:dns ok\ncheck:operator ok\n");
        let now = lines(
            "check:claude-login fail\ncheck:dns ok\ncheck:operator fail\nuser-unit:claude-rc@consolebook.service fail\nuser-unit:claude-rc@web.service fail\n",
        );
        let (bad, excused) = judge(&before, &now, &explains);
        assert_eq!(bad, Vec::<String>::new());
        assert_eq!(
            excused,
            [
                "check:operator is failing, but not counted: check:claude-login is failing, and it can't pass without that",
                "claude-rc@consolebook.service failed, but not counted: check:claude-login is failing, and it can't pass without that",
                "claude-rc@web.service failed, but not counted: check:claude-login is failing, and it can't pass without that",
            ]
        );
        // Without the explanation, the same reports revert the switch.
        assert_eq!(
            regressions(&before, &now),
            [
                "check:operator is failing",
                "claude-rc@consolebook.service failed",
                "claude-rc@web.service failed",
            ]
        );
        // A switch that breaks the login itself still regresses, and what
        // needs it is said apart. What it doesn't explain counts as ever:
        // the system's unit of the same name, and another check.
        let before = lines("check:claude-login ok\ncheck:dns ok\ncheck:operator ok\n");
        let now = lines(
            "check:claude-login fail\ncheck:dns fail\ncheck:operator fail\nunit:claude-rc@web.service fail\nuser-unit:claude-rc@web.service fail\n",
        );
        let (bad, excused) = judge(&before, &now, &explains);
        assert_eq!(
            bad,
            [
                "check:claude-login is failing",
                "check:dns is failing",
                "claude-rc@web.service failed",
            ]
        );
        assert_eq!(excused.len(), 2, "{excused:?}");
        // With the login back, nothing is excused.
        let now = lines("check:claude-login ok\ncheck:dns ok\ncheck:operator fail\n");
        assert_eq!(
            judge(&before, &now, &explains),
            (vec!["check:operator is failing".to_owned()], vec![])
        );
        // A check never explains itself away.
        let explains = BTreeMap::from([("claude-login".to_owned(), vec!["check:*".to_owned()])]);
        let now = lines("check:claude-login fail\ncheck:dns ok\ncheck:operator fail\n");
        assert_eq!(
            judge(&before, &now, &explains).0,
            ["check:claude-login is failing"]
        );
    }

    #[test]
    fn a_pattern_stands_for_what_it_says() {
        for (pattern, key, is) in [
            ("check:operator", "check:operator", true),
            ("check:operator", "check:operators", false),
            ("check:operator", "check:operato", false),
            (
                "user-unit:claude-rc@*.service",
                "user-unit:claude-rc@web.service",
                true,
            ),
            (
                "user-unit:claude-rc@*.service",
                "user-unit:claude-rc@.service",
                true,
            ),
            (
                "user-unit:claude-rc@*.service",
                "unit:claude-rc@web.service",
                false,
            ),
            (
                "user-unit:claude-rc@*.service",
                "user-unit:claude-rc@web.timer",
                false,
            ),
            ("*", "", true),
            ("a*b*c", "abc", true),
            ("a*b*c", "aXbYbZc", true),
            ("a*b*c", "acb", false),
            ("ab*ba", "aba", false),
            ("a.c", "abc", false),
        ] {
            assert_eq!(glob(pattern, key), is, "{pattern} {key}");
        }
    }
}
