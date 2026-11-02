//! Live, with the dead man's switch; healthy, or straight back. A system a
//! run found live already went through this when it went live.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::dispatch::{open, settled};
use crate::run::{Run, Step, name};
use crate::status::Outcome;
use crate::{Broke, train};

/// How long the health check waits for the box to settle after a switch,
/// in seconds.
const SETTLE: &str = "180";

/// What the switch exits with when it refuses a system as built from an
/// older nixpkgs than the running one, before anything went live.
const DOWNGRADE: i32 = 3;

impl Run<'_> {
    pub(crate) fn go_live(&mut self) -> Step {
        if self.live {
            return Ok(());
        }
        let cfg = self.cfg;
        let tools = &cfg.tools;
        let log = cfg.state.join("logs/switch.log");
        let before = cfg.state.join("health-before");
        let snapped = Command::new(&tools.health)
            .arg("snapshot")
            .arg(&before)
            .status()
            .is_ok_and(|s| s.success());
        if !snapped {
            return Err(Broke(format!("{} snapshot: failed", name(&tools.health))).into());
        }
        let claude_before = self.claude_version();
        let (out, err) = to(&log, false)?;
        let switched = Command::new(&tools.switch)
            .env("BURROW_CONFIRM_WITHIN", &cfg.within)
            .args(["--toplevel", &self.new])
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err)
            .status()
            .ok();
        let switch = name(&tools.switch);
        let what = self.what()?;
        match switched.and_then(|s| if s.success() { Some(0) } else { s.code() }) {
            Some(0) => {}
            Some(DOWNGRADE) => {
                let e = self.end(
                    Outcome::Failed,
                    "the switch refused it as a downgrade; nothing changed",
                );
                self.dispatch(
                    "switch",
                    &format!(
                        "{}: {switch} refused {} ({what}) as built from an older nixpkgs than the running system, so nothing went live. The output is {}. Find out why the new system looks older (each reads nixpkgs' date from its /etc/burrow/inputs.json) and fix it. {}",
                        cfg.update,
                        self.new,
                        log.display(),
                        self.rules
                    ),
                );
                return Err(e);
            }
            _ => {
                self.back(&log);
                let e = self.end(Outcome::Failed, "activation failed; reverted");
                self.dispatch(
                    "activation",
                    &format!(
                        "{}: switching to {} ({what}) failed during activation and was reverted. The output is {}; journalctl has the rest. Fix it. {}",
                        cfg.update,
                        self.new,
                        log.display(),
                        self.rules
                    ),
                );
                return Err(e);
            }
        }
        let compared = Command::new(&tools.health)
            .arg("compare")
            .arg(&before)
            .arg(SETTLE)
            .stdin(Stdio::null())
            .output();
        if let Some(bad) = compared.map_or(Some(String::new()), |o| {
            (!o.status.success()).then(|| {
                String::from_utf8_lossy(&o.stdout)
                    .trim_end_matches('\n')
                    .to_string()
            })
        }) {
            self.back(&log);
            let e = self.end(
                Outcome::Failed,
                &format!("unhealthy after the switch ({bad}); reverted"),
            );
            self.dispatch(
                "health",
                &format!(
                    "{} switched {} to {} ({what}) and these regressed: {bad}. It reverted right away. Find out why (journalctl, the unit logs, a diff of the two systems: nix store diff-closures {} {}) and fix it. {}",
                    cfg.update,
                    cfg.host,
                    self.new,
                    self.running.current,
                    self.new,
                    self.rules
                ),
            );
            open(
                cfg,
                "health",
                "An update broke something and was rolled back",
                &format!(
                    "{} switched to {what} and then: {bad}. {} went straight back to the previous system. A Claude job is on it; this closes when an update goes through.",
                    cfg.update, cfg.host
                ),
            );
            return Err(e);
        }

        // Claude Code itself: if it changed, it still answers. A failure
        // here is noted, not reverted: it can be Anthropic's side as easily
        // as ours.
        if self.claude_version() != claude_before && !self.claude_answers() {
            self.warn("warning: the new Claude Code didn't answer a one-line prompt");
        }

        // A fresh bump for its red PR goes up as that PR's head before it's
        // confirmed: refused (a fix or a rerun got there first), it goes
        // straight back, so the box never stays on what nothing will land,
        // and the next run rides or holds for that PR as usual.
        if let (Some(t), Some(pr)) = (&cfg.train, &self.replacing) {
            match train::replace(cfg, t, self.api, pr) {
                Ok(url) => self.url = Some(url),
                Err(why) => {
                    self.back(&log);
                    return Err(self.end(Outcome::Held, &format!("{why}; reverted")));
                }
            }
        }

        let (out, err) = to(&log, true)?;
        let confirmed = Command::new(&tools.confirm)
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err)
            .status()
            .is_ok_and(|s| s.success());
        if !confirmed {
            return Err(self.end(
                Outcome::Failed,
                &format!(
                    "health was fine but {} failed; it reverts by itself",
                    name(&tools.confirm)
                ),
            ));
        }
        for key in ["switch", "activation", "health"] {
            settled(cfg, key, "An update went through.");
        }
        if self.untested.is_empty() {
            settled(cfg, "untested", "An update passed every VM test again.");
        }
        Ok(())
    }

    /// The switch taken back, saying so in its log: whatever that says, the
    /// dead man's switch reverts it in the end.
    fn back(&self, log: &Path) {
        let mut c = Command::new(&self.cfg.tools.switch);
        c.arg("--abort").stdin(Stdio::null());
        if let Ok((out, err)) = to(log, true) {
            c.stdout(out).stderr(err);
        }
        let _ = c.status();
    }

    /// Claude Code's version, as the user runs it: nothing when it doesn't
    /// say.
    fn claude_version(&self) -> String {
        self.cfg
            .run_as_user("claude")
            .arg("--version")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .trim_end_matches('\n')
                    .to_string()
            })
            .unwrap_or_default()
    }

    /// Whether Claude Code answers a one-line prompt, in the user's home,
    /// within two minutes.
    fn claude_answers(&self) -> bool {
        let out = self
            .cfg
            .run_as_user("timeout")
            .args([
                "120",
                "claude",
                "-p",
                "Reply with the single word OK.",
                "--max-turns",
                "1",
            ])
            .current_dir(&self.cfg.home)
            .stdin(Stdio::null())
            .output();
        out.is_ok_and(|o| {
            String::from_utf8_lossy(&o.stdout).contains("OK")
                || String::from_utf8_lossy(&o.stderr).contains("OK")
        })
    }

    /// `line`, in the journal as a warning of HOST-update's.
    fn warn(&self, line: &str) {
        let cfg = self.cfg;
        let logged = Command::new(&cfg.tools.logger)
            .args(["-t", &cfg.update, "-p", "warning"])
            .stdin(Stdio::piped())
            .spawn()
            .and_then(|mut c| {
                if let Some(mut stdin) = c.stdin.take() {
                    writeln!(stdin, "{line}")?;
                }
                c.wait()
            });
        if !logged.is_ok_and(|s| s.success()) {
            eprintln!("{line}");
        }
    }
}

/// The switch's log, for a program's stdout and stderr both, as `>LOG 2>&1`
/// has it: from scratch, or after what's in it (`>>`).
fn to(log: &Path, append: bool) -> Result<(File, File), Broke> {
    let broke = |e: std::io::Error| Broke(format!("{}: {e}", log.display()));
    let out = OpenOptions::new()
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append)
        .open(log)
        .map_err(broke)?;
    let err = out.try_clone().map_err(broke)?;
    Ok((out, err))
}
