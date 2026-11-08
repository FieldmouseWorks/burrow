//! Work completion events retry queued trials; the daily timer is a backstop.
use crate::{Broke, Config};
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub fn run(cfg: &Config) -> Result<u8, Broke> {
    let mut child = Command::new(&cfg.tools.journalctl)
        .args(["--follow", "--lines=0", "--quiet", "--output=json"])
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| Broke(e.to_string()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Broke("journalctl gave no stdout".into()))?;
    let (tx, rx) = mpsc::channel();
    let work = Work::of(cfg);
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else {
                break;
            };
            if wakes(&line, &work) && tx.send(()).is_err() {
                break;
            }
        }
    });
    kick(cfg)?;
    while rx.recv().is_ok() {
        let until = Instant::now() + Duration::from_secs(2);
        while let Some(left) = until.checked_duration_since(Instant::now()) {
            if rx.recv_timeout(left).is_err() {
                break;
            }
        }
        kick(cfg)?;
    }
    let status = child.wait().map_err(|e| Broke(e.to_string()))?;
    Err(Broke(format!("journalctl stopped following: {status}")))
}
fn kick(cfg: &Config) -> Result<(), Broke> {
    if !cfg.state.join(crate::boot::TRIAL).exists() {
        return Ok(());
    }
    let status = Command::new(&cfg.tools.systemctl)
        .args([
            "start",
            "--no-block",
            &format!("{}-trial.service", cfg.boot),
        ])
        .status()
        .map_err(|e| Broke(e.to_string()))?;
    if status.success() {
        Ok(())
    } else {
        Err(Broke(format!("start trial: {status}")))
    }
}
/// The units whose ending can make the box idle, as `busy` sees them.
struct Work {
    user: Vec<String>,
    system: Vec<String>,
    runners: Vec<String>,
}

impl Work {
    fn of(cfg: &Config) -> Work {
        Work {
            user: cfg.user_work(),
            system: cfg.system_work(),
            runners: cfg.runners.clone(),
        }
    }
}

/// systemd's message ids for a unit that stopped, failed, finished or was
/// accounted for at its end. A start never makes the box idle, so it's
/// left out.
const ENDED: [&str; 5] = [
    "9d1aaa27d60140bd96365438aad20286", // UNIT_STOPPED
    "be02cf6855d2428ba40df7e9d022f03d", // UNIT_FAILED
    "7ad2d189f7e94e70a38c781354912448", // UNIT_SUCCESS
    "d9b373ed55a64feb8242e02dbe79a49c", // UNIT_FAILURE_RESULT
    "ae8f7b866b0347b9af31fe1c80b127c0", // UNIT_RESOURCES
];

/// Whether a journal entry (JSON) is work `busy` waits for ending: a unit
/// of its own ending, as its service manager says (`_SYSTEMD_UNIT` and
/// `_SYSTEMD_USER_UNIT` are the journal's, so no one else's log can claim
/// them), or a runner saying its job completed. Nothing else wakes it, so
/// the units and sessions a trial's own `busy` starts can't start it again.
fn wakes(line: &str, work: &Work) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
        return false;
    };
    let text = |key: &str| v.get(key).and_then(serde_json::Value::as_str).unwrap_or("");
    let any = |globs: &[String], name: &str| globs.iter().any(|g| glob(g, name));
    let manager = text("_SYSTEMD_UNIT");
    if ENDED.contains(&text("MESSAGE_ID")) {
        if manager == "init.scope" {
            let unit = text("UNIT");
            return any(&work.system, unit) || any(&work.runners, unit);
        }
        if manager.starts_with("user@") && text("_SYSTEMD_USER_UNIT") == "init.scope" {
            return any(&work.user, text("USER_UNIT"));
        }
        return false;
    }
    any(&work.runners, manager) && text("MESSAGE").contains("completed with result:")
}

/// systemctl's unit globs, as burrow writes them: `*` is any run of
/// characters, `?` any one, and the rest stands for itself.
fn glob(pattern: &str, name: &str) -> bool {
    let (p, n): (Vec<char>, Vec<char>) = (pattern.chars().collect(), name.chars().collect());
    let (mut i, mut j) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while j < n.len() {
        match p.get(i) {
            Some('*') => {
                star = Some((i, j));
                i += 1;
            }
            Some(&c) if c == '?' || c == n[j] => {
                i += 1;
                j += 1;
            }
            _ => match star {
                Some((si, sj)) => {
                    star = Some((si, sj + 1));
                    i = si + 1;
                    j = sj + 1;
                }
                None => return false,
            },
        }
    }
    p[i..].iter().all(|&c| c == '*')
}

#[cfg(test)]
mod tests {
    use super::{Work, glob, wakes};

    fn work() -> Work {
        Work {
            user: vec!["proj-*.service".into(), "autoland-proof-*.service".into()],
            system: vec![
                "box-update.service".into(),
                "autoland*.service".into(),
                "microvm@*.service".into(),
            ],
            runners: vec!["ci-runner.service".into(), "fresh-runner@*.service".into()],
        }
    }

    #[test]
    fn the_work_busy_waits_for_ending_wakes_it() {
        let w = work();
        for line in [
            // A Claude job exiting by itself: the user manager accounts for it.
            r#"{"MESSAGE_ID":"ae8f7b866b0347b9af31fe1c80b127c0","USER_UNIT":"proj-burrow\\x2d1.service","_SYSTEMD_UNIT":"user@1001.service","_SYSTEMD_USER_UNIT":"init.scope"}"#,
            r#"{"MESSAGE_ID":"9d1aaa27d60140bd96365438aad20286","USER_UNIT":"autoland-proof-x.service","_SYSTEMD_UNIT":"user@1001.service","_SYSTEMD_USER_UNIT":"init.scope"}"#,
            r#"{"MESSAGE_ID":"7ad2d189f7e94e70a38c781354912448","UNIT":"autoland@a-b.service","_SYSTEMD_UNIT":"init.scope"}"#,
            r#"{"MESSAGE_ID":"d9b373ed55a64feb8242e02dbe79a49c","UNIT":"box-update.service","_SYSTEMD_UNIT":"init.scope"}"#,
            r#"{"MESSAGE_ID":"be02cf6855d2428ba40df7e9d022f03d","UNIT":"microvm@web.service","_SYSTEMD_UNIT":"init.scope"}"#,
            r#"{"MESSAGE_ID":"7ad2d189f7e94e70a38c781354912448","UNIT":"fresh-runner@7-0.service","_SYSTEMD_UNIT":"init.scope"}"#,
            r#"{"MESSAGE":"2026-11-08 19:49:27Z: Job check completed with result: Succeeded","_SYSTEMD_UNIT":"ci-runner.service"}"#,
        ] {
            assert!(wakes(line, &w), "{line}");
        }
    }

    #[test]
    fn what_a_trial_itself_starts_and_other_units_do_not_wake_it() {
        let w = work();
        for line in [
            // What a postponed trial's own `systemctl --user -M USER@` logs.
            r#"{"MESSAGE":"Started [systemd-run] systemd-stdio-bridge --user --quiet.","MESSAGE_ID":"39f53479d3a045ac8e11786248231fbf","UNIT":"run-p675876-i63576323.service","_SYSTEMD_UNIT":"init.scope"}"#,
            r#"{"MESSAGE":"Started Session 4542 of User dev.","MESSAGE_ID":"39f53479d3a045ac8e11786248231fbf","UNIT":"session-4542.scope","_SYSTEMD_UNIT":"init.scope"}"#,
            r#"{"MESSAGE_ID":"7ad2d189f7e94e70a38c781354912448","UNIT":"run-p675876-i63576323.service","_SYSTEMD_UNIT":"init.scope"}"#,
            r#"{"MESSAGE_ID":"ae8f7b866b0347b9af31fe1c80b127c0","UNIT":"session-4542.scope","_SYSTEMD_UNIT":"init.scope"}"#,
            // The trial's own unit.
            r#"{"MESSAGE_ID":"9d1aaa27d60140bd96365438aad20286","UNIT":"box-boot-trial.service","_SYSTEMD_UNIT":"init.scope"}"#,
            // Work starting never makes the box idle.
            r#"{"MESSAGE_ID":"39f53479d3a045ac8e11786248231fbf","UNIT":"autoland@a-b.service","_SYSTEMD_UNIT":"init.scope"}"#,
            // A user's unit name on the system manager, and the reverse.
            r#"{"MESSAGE_ID":"9d1aaa27d60140bd96365438aad20286","UNIT":"proj-1.service","_SYSTEMD_UNIT":"init.scope"}"#,
            r#"{"MESSAGE_ID":"9d1aaa27d60140bd96365438aad20286","USER_UNIT":"autoland@a.service","_SYSTEMD_UNIT":"user@1001.service","_SYSTEMD_USER_UNIT":"init.scope"}"#,
            // Someone else's log claiming a work unit.
            r#"{"MESSAGE_ID":"9d1aaa27d60140bd96365438aad20286","UNIT":"autoland@a.service","_SYSTEMD_UNIT":"user@1001.service"}"#,
            // The runner's words from anything but a runner (a grep for them, say).
            r#"{"MESSAGE":"COMMAND=journalctl --grep 'completed with result:'","_SYSTEMD_UNIT":"user@1001.service"}"#,
            r#"{"MESSAGE":"unrelated","_SYSTEMD_UNIT":"ci-runner.service"}"#,
            "not json",
        ] {
            assert!(!wakes(line, &w), "{line}");
        }
    }

    #[test]
    fn globs_match_as_systemctl_does() {
        assert!(glob("proj-*.service", "proj-a\\x2db.service"));
        assert!(glob("autoland*.service", "autoland.service"));
        assert!(glob("a?c*", "abcdef"));
        assert!(glob("*x*y", "axbxy"));
        assert!(!glob("proj-*.service", "proj-a.scope"));
        assert!(!glob("box-update.service", "box-update.services"));
        assert!(!glob("autoland*.service", "run-autoland.service"));
    }
}
