//! HOST-switch: the running system changed with a dead man's switch.
//!
//! The new system goes live at once but is NOT made the boot default.
//! Unless HOST-confirm runs within the window, the previous system is
//! switched back in by itself. If the box hangs outright, a hardware
//! watchdog (if it has one) reboots it into the last confirmed generation.
//! So a change that cuts off SSH, Tailscale or Claude itself undoes itself.
//! An activation that hangs is killed three quarters of the way through the
//! window and reverted at once, so the old system is back before the window
//! ends.
//!
//! It won't put a system live whose nixpkgs is older than the running one's
//! (that would quietly undo security fixes) unless told
//! `--allow-downgrade`; it knows nixpkgs' date from /etc/burrow/inputs.json,
//! which burrow.switch.flakeInputs writes.

use std::fs::{self, OpenOptions};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rustix::fs::{Access, FlockOperation, access, flock};

use crate::{Broke, Config, canonical, complain, exit_of, ran, say};

/// What a switch exits with when it refuses a downgrade, before anything
/// changed (HOST-update tells it by this).
pub const DOWNGRADE: u8 = 3;

/// What switch-to-configuration exits with when someone else holds its
/// lock: it changed nothing.
const LOCKED: i32 = 11;

/// systemd-oomd (burrow.ops.oomd) never picks the switch's own units: a
/// killed revert would leave the unconfirmed system live with nothing left
/// to undo it, and a killed activation leaves it half switched.
const KEEP: &str = "--property=ManagedOOMPreference=omit";

#[derive(Debug, PartialEq, Eq)]
pub enum Word {
    /// Put a system live: built from a flake (the machine's, unless one's
    /// named), or one already built (`--toplevel`).
    Switch {
        system: System,
        allow_downgrade: bool,
    },
    /// What's pending.
    Status,
    /// A pending switch reverted now instead of waiting.
    Abort,
    /// What the revert's units run: the confirmed system put back.
    RevertTo(PathBuf),
    /// What the activation's unit runs as it stops, with the system it was
    /// putting live.
    ActivationStopped(PathBuf),
}

#[derive(Debug, PartialEq, Eq)]
pub enum System {
    Flake(Option<PathBuf>),
    Toplevel(PathBuf),
}

/// Its words, `--allow-downgrade` anywhere among them: nothing when they
/// aren't.
pub fn parse(words: &[String]) -> Option<Word> {
    let allow_downgrade = words.iter().any(|w| w == "--allow-downgrade");
    let words: Vec<&str> = words
        .iter()
        .map(String::as_str)
        .filter(|w| *w != "--allow-downgrade")
        .collect();
    let path = |p: &str| (!p.is_empty()).then(|| PathBuf::from(p));
    let switch = |system| Word::Switch {
        system,
        allow_downgrade,
    };
    Some(match words[..] {
        [] => switch(System::Flake(None)),
        ["--status"] => Word::Status,
        ["--abort"] => Word::Abort,
        ["--toplevel", p] => switch(System::Toplevel(path(p)?)),
        ["--revert-to", p] => Word::RevertTo(path(p)?),
        ["--activation-stopped", p] => Word::ActivationStopped(path(p)?),
        [dir] if !dir.starts_with('-') => switch(System::Flake(Some(path(dir)?))),
        _ => return None,
    })
}

/// How a unit runs HOST-switch itself: this binary, with its config.
pub struct Me {
    pub exe: PathBuf,
    pub config: PathBuf,
}

impl Me {
    /// As a command line, for a unit's ExecStopPost.
    fn line(&self, words: &[&Path]) -> String {
        let mut line = format!("{} {}", self.exe.display(), self.config.display());
        for w in words {
            line.push(' ');
            line.push_str(&w.to_string_lossy());
        }
        line
    }
}

/// Runs `word`, with `within` the window a switch waits for its confirm:
/// what it exits with, or why it broke.
pub fn run(cfg: &Config, me: &Me, within: &str, word: &Word) -> Result<u8, Broke> {
    match word {
        Word::Switch {
            system,
            allow_downgrade,
        } => switch(cfg, me, within, system, *allow_downgrade),
        Word::Status => status(cfg),
        Word::Abort => abort(cfg, me),
        Word::RevertTo(prev) => revert_to(cfg, prev),
        Word::ActivationStopped(new) => activation_stopped(cfg, new),
    }
}

fn switch(
    cfg: &Config,
    me: &Me,
    within: &str,
    system: &System,
    allow_downgrade: bool,
) -> Result<u8, Broke> {
    // As systemd reads the window, before anything: the revert's timer and
    // the activation's deadline both come from it.
    let Some(window) = microseconds(cfg, within) else {
        complain(&format!(
            "{}: {within}: not a time span systemd reads",
            cfg.switch
        ));
        return Ok(2);
    };
    let new = match system {
        System::Toplevel(path) => fs::canonicalize(path).unwrap_or_else(|_| path.clone()),
        System::Flake(dir) => build(cfg, dir.as_deref().unwrap_or(&cfg.flake))?,
    };
    if access(new.join("bin/switch-to-configuration"), Access::EXEC_OK).is_err() {
        complain(&format!("not a NixOS system: {}", new.display()));
        return Ok(1);
    }
    if new == cfg.running()? {
        say(&format!("already running {}", new.display()));
        return Ok(0);
    }
    if !allow_downgrade && stamp(&new) < stamp(&cfg.current) {
        complain(&format!(
            "refusing: {} is built from an older nixpkgs than the running system",
            new.display()
        ));
        complain(
            "(that would undo security fixes; pull origin/main first, or pass --allow-downgrade)",
        );
        return Ok(DOWNGRADE);
    }

    let _lock = cfg.lock()?;
    // The last *confirmed* system, even if an earlier switch is still
    // pending: read under the lock, as a confirm may have just moved it.
    let prev = canonical(&cfg.profile)?;
    // Any earlier pending switch disarmed; if its revert already fired, it
    // lands rather than being interrupted halfway through activating.
    cfg.systemctl_quiet(&["stop", &cfg.timer()]);
    cfg.wait_while(Config::reverting);
    let before = Before::read(cfg)?;
    cfg.systemctl_quiet(&["reset-failed", &cfg.timer(), &cfg.service()]);
    cfg.remove("revert-failed")?;
    cfg.write("prev", &prev)?;
    cfg.write("pending", &new)?;

    // The revert armed before anything is touched.
    if let Err(e) = arm(cfg, me, within, &prev) {
        // Nothing went live.
        not_armed(cfg, me, within, &prev, &before)?;
        return Err(e);
    }

    // Activating in a unit of its own, so it finishes even if it restarts
    // whatever called it. switch-to-configuration holds the lock the revert
    // needs, so a hung activation would hold off the revert for good: it
    // gets three quarters of the window, and the rest is the revert's.
    // Killing it halfway leaves nothing behind: the revert picks up the
    // units it stopped from the list switch-to-configuration keeps in
    // /run/nixos, a start that never ends is stopped by the revert's own
    // activation, and tests/switch.nix holds them to that.
    let activated = Command::new(&cfg.tools.systemd_run)
        .arg(format!("--unit={}-activate", cfg.switch))
        .args([KEEP, "--collect", "--wait", "--pipe", "--quiet"])
        .arg(format!("--property=RuntimeMaxSec={}us", window * 3 / 4))
        .arg("--property=TimeoutStopSec=5s")
        .arg(format!(
            "--property=ExecStopPost={}",
            me.line(&[Path::new("--activation-stopped"), &new])
        ))
        .arg(new.join("bin/switch-to-configuration"))
        .arg("test")
        .stdin(Stdio::null())
        .status();
    let prev = prev.display();
    // The timer's gone once the revert has fired or the deadline started
    // it: the old system is coming back whatever the activation said.
    if cfg.show(&cfg.timer(), "SubState") != "waiting" {
        complain(&format!(
            "not kept: activating took too long for a {within} window, so it reverts to {prev}"
        ));
        return Ok(1);
    }
    if exit_of(activated) != 0 {
        complain(&format!(
            "activation reported errors; it will revert in {within} unless you run {}",
            cfg.confirm
        ));
        return Ok(1);
    }
    say(&format!("live: {}", new.display()));
    say(&format!(
        "run {} within {within} to keep it (and make it the boot default)",
        cfg.confirm
    ));
    Ok(0)
}

/// The revert's timer: back to `prev` in `within`.
fn arm(cfg: &Config, me: &Me, within: &str, prev: &Path) -> Result<(), Broke> {
    ran(Command::new(&cfg.tools.systemd_run)
        .arg(format!("--unit={}", cfg.revert))
        .arg(format!("--on-active={within}"))
        .args([
            "--timer-property=AccuracySec=1s",
            "--property=Type=oneshot",
            KEEP,
        ])
        .arg(format!("--setenv=PATH={}", cfg.path))
        .arg(&me.exe)
        .arg(&me.config)
        .arg("--revert-to")
        .arg(prev))
}

/// What was pending before a switch, and whether its revert had failed.
struct Before {
    pending: Option<PathBuf>,
    revert_failed: Option<PathBuf>,
}

impl Before {
    fn read(cfg: &Config) -> Result<Before, Broke> {
        Ok(Before {
            pending: cfg.recorded("pending")?,
            revert_failed: cfg.recorded("revert-failed")?,
        })
    }
}

/// A switch whose revert won't arm, taken back. With nothing unconfirmed
/// live, nothing's pending. Otherwise what was pending is again, and an
/// earlier switch that's still live keeps a revert: the one this switch
/// stopped, armed again with its window, or, when that won't arm either,
/// the confirmed system put back now, in a unit of its own as an abort's.
fn not_armed(
    cfg: &Config,
    me: &Me,
    within: &str,
    prev: &Path,
    before: &Before,
) -> Result<(), Broke> {
    let running = cfg.running()?;
    if running == prev {
        return cfg.remove("pending");
    }
    for (name, was) in [
        ("pending", &before.pending),
        ("revert-failed", &before.revert_failed),
    ] {
        match was {
            Some(system) => cfg.write(name, system)?,
            None => cfg.remove(name)?,
        }
    }
    // An earlier switch's revert that had fired and failed already stays
    // said, by status; one whose timer wasn't stopped still waits.
    if before.pending.as_deref() != Some(running.as_path())
        || before.revert_failed.is_some()
        || cfg.show(&cfg.timer(), "SubState") == "waiting"
    {
        return Ok(());
    }
    let running = running.display();
    if arm(cfg, me, within, prev).is_ok() {
        complain(&format!(
            "{running} is still pending: it reverts in {within} unless you run {}",
            cfg.confirm
        ));
        return Ok(());
    }
    complain(&format!(
        "{running} is still unconfirmed, and its revert won't arm either: reverting to {} now",
        prev.display()
    ));
    let _ = revert_now(cfg, me, prev);
    // A revert that ran says itself how it went. One that couldn't start
    // is said as one that didn't get back.
    if cfg.running()? != prev && cfg.recorded("revert-failed")?.is_none() {
        cfg.write("revert-failed", prev)?;
        cfg.journal(
            "err",
            &format!("REVERT FAILED (it couldn't start): still running the unconfirmed {running}"),
        );
    }
    Ok(())
}

/// The window, in microseconds, as `systemd-analyze timespan` reads it:
/// nothing when it can't.
fn microseconds(cfg: &Config, within: &str) -> Option<u64> {
    let out = Command::new(&cfg.tools.systemd_analyze)
        .args(["timespan", within])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    // Its "μs:" line, which says "us:" outside a UTF-8 locale.
    String::from_utf8_lossy(&out.stdout).lines().find_map(|l| {
        let mut words = l.split_whitespace();
        match (words.next(), words.next()) {
            (Some(key), Some(value)) if key.ends_with("s:") => value.parse().ok(),
            _ => None,
        }
    })
}

/// The flake in `dir` built, as its checkout's owner (git refuses repos
/// owned by someone else): the system it built.
fn build(cfg: &Config, dir: &Path) -> Result<PathBuf, Broke> {
    let flake = canonical(dir)?;
    let owner = fs::metadata(&flake)
        .map_err(|e| crate::at(&flake, e))?
        .uid();
    // runuser, not sudo: under systemd-run there are no setuid wrappers.
    let mut c = if owner == rustix::process::geteuid().as_raw() {
        Command::new(&cfg.tools.nix)
    } else {
        let mut c = Command::new(&cfg.tools.runuser);
        c.args(["-u", &user_name(owner)?, "--"]).arg(&cfg.tools.nix);
        c
    };
    c.args(["build", "--no-link", "--print-out-paths"])
        .arg(format!(
            "{}#nixosConfigurations.{}.config.system.build.toplevel",
            flake.display(),
            cfg.configuration
        ));
    let out = c
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| Broke(format!("{}: {e}", crate::shown(&c))))?;
    if !out.status.success() {
        return Err(Broke(format!("{}: {}", crate::shown(&c), out.status)));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().into())
}

/// The name of the user `uid`, from /etc/passwd.
fn user_name(uid: u32) -> Result<String, Broke> {
    let passwd = Path::new("/etc/passwd");
    let text = fs::read_to_string(passwd).map_err(|e| crate::at(passwd, e))?;
    named(&text, uid).ok_or_else(|| Broke(format!("no user {uid} in /etc/passwd")))
}

/// Who `uid` is in `passwd`, a passwd(5) file.
fn named(passwd: &str, uid: u32) -> Option<String> {
    passwd.lines().find_map(|l| {
        let f: Vec<&str> = l.split(':').collect();
        (f.len() > 2 && f[2] == uid.to_string()).then(|| f[0].to_string())
    })
}

/// nixpkgs' commit time, as built into `system` (burrow.switch.flakeInputs):
/// 0 when it doesn't say.
fn stamp(system: &Path) -> i64 {
    fs::read(system.join("etc/burrow/inputs.json"))
        .ok()
        .and_then(|t| serde_json::from_slice::<serde_json::Value>(&t).ok())
        .and_then(|v| v["nixpkgs"]["lastModified"].as_i64())
        .unwrap_or(0)
}

fn status(cfg: &Config) -> Result<u8, Broke> {
    let shown = |name| {
        cfg.recorded(name)
            .ok()
            .flatten()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    };
    if cfg.file("revert-failed").exists() {
        say(&format!(
            "REVERT FAILED: still on the unconfirmed {};",
            cfg.running()?.display()
        ));
        say(&format!(
            "the last confirmed system is {} (see journalctl -t {})",
            shown("revert-failed"),
            cfg.switch
        ));
    }
    if cfg.reverting() {
        say(&format!("reverting to {} right now", shown("prev")));
    } else if let Some(pending) = cfg.recorded("pending")? {
        say(&format!("pending: {}", pending.display()));
        say(&format!("reverts to: {}", shown("prev")));
        // When it goes off.
        let timers = Command::new(&cfg.tools.systemctl)
            .args(["list-timers", &cfg.timer(), "--no-pager"])
            .stdin(Stdio::null())
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        if let Some(line) = timers.lines().nth(1) {
            say(line);
        }
    } else {
        say(&format!(
            "nothing pending; running {}",
            cfg.running()?.display()
        ));
    }
    Ok(0)
}

fn abort(cfg: &Config, me: &Me) -> Result<u8, Broke> {
    let _lock = cfg.lock()?;
    if cfg.recorded("pending")?.is_none() {
        say("nothing pending");
        return Ok(0);
    }
    cfg.systemctl_quiet(&["stop", &cfg.timer()]);
    // A revert that already fired finishes on its own.
    cfg.wait_while(Config::reverting);
    if cfg.recorded("pending")?.is_none() {
        say("already reverted");
        return Ok(0);
    }
    let Some(prev) = cfg.recorded("prev")? else {
        return Err(Broke(format!(
            "{}: no confirmed system to go back to",
            cfg.file("prev").display()
        )));
    };
    Ok(u8::try_from(exit_of(revert_now(cfg, me, &prev))).unwrap_or(1))
}

/// The revert, now, in a unit of its own: it outlives whatever started it,
/// even when putting `prev` back stops that.
fn revert_now(cfg: &Config, me: &Me, prev: &Path) -> std::io::Result<std::process::ExitStatus> {
    Command::new(&cfg.tools.systemd_run)
        .arg(format!("--unit={}-now", cfg.revert))
        .args([KEEP, "--collect", "--wait", "--pipe", "--quiet"])
        .arg(format!("--setenv=PATH={}", cfg.path))
        .arg(&me.exe)
        .arg(&me.config)
        .arg("--revert-to")
        .arg(prev)
        .status()
}

/// The confirmed system put back, as the revert's units run it when the
/// switch wasn't confirmed in time, or was aborted.
fn revert_to(cfg: &Config, prev: &Path) -> Result<u8, Broke> {
    // switch-to-configuration holds its lock while it activates, and
    // refuses at once (exit 11) while someone else holds it: the switch
    // being undone, when activating outlasts the window, or a confirm
    // writing the boot default. A refused activation changed nothing, and
    // the running system may not have moved yet, so that one is waited out
    // and prev put back.
    let rc = loop {
        if let Ok(lock) = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&cfg.activation_lock)
        {
            let _ = flock(&lock, FlockOperation::LockExclusive);
        }
        let rc = exit_of(
            Command::new(prev.join("bin/switch-to-configuration"))
                .arg("test")
                .stdin(Stdio::null())
                .status(),
        );
        if rc != LOCKED {
            break rc;
        }
    };
    // A unit failing to restart still counts as switched back (exit 4):
    // what matters is which system is live now.
    let running = fs::canonicalize(&cfg.current).unwrap_or_else(|_| cfg.current.clone());
    if running == prev {
        cfg.remove("pending")?;
        cfg.remove("revert-failed")?;
        cfg.journal(
            "warning",
            &format!(
                "not confirmed, reverted to {} (activation exit {rc})",
                prev.display()
            ),
        );
        Ok(0)
    } else {
        cfg.write("revert-failed", prev)?;
        cfg.journal(
            "err",
            &format!(
                "REVERT FAILED (exit {rc}): still running the unconfirmed {}",
                running.display()
            ),
        );
        Ok(1)
    }
}

/// Run as the activation's unit stops. Past its deadline, if the timer
/// hasn't fired yet, the revert starts now: the timer would land it after
/// the window. A switch no longer pending (confirmed, aborted, reverted, or
/// replaced by a later switch) is settled: left be. A stop job the killed
/// activation left behind still holds up the revert's own job on that unit
/// until systemd's stop timeout ends it; a unit with none
/// (TimeoutStopSec=infinity) holds it for good, burrow#312.
fn activation_stopped(cfg: &Config, new: &Path) -> Result<u8, Broke> {
    let timed_out = std::env::var_os("SERVICE_RESULT").is_some_and(|r| r == "timeout");
    if timed_out
        && cfg.recorded("pending").ok().flatten().as_deref() == Some(new)
        && cfg.show(&cfg.timer(), "SubState") == "waiting"
    {
        cfg.journal(
            "warning",
            "activation killed: it hung past its deadline, reverting now",
        );
        // In this order: with the timer gone first, nothing would hold the
        // inactive transient service loaded.
        ran(Command::new(&cfg.tools.systemctl)
            .args(["start", "--no-block"])
            .arg(cfg.service()))?;
        ran(Command::new(&cfg.tools.systemctl)
            .arg("stop")
            .arg(cfg.timer()))?;
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(words: &[&str]) -> Option<Word> {
        parse(&words.iter().map(|w| w.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn its_words_are_a_switch_its_status_an_abort_and_its_units() {
        let switch = |system, allow_downgrade| {
            Some(Word::Switch {
                system,
                allow_downgrade,
            })
        };
        assert_eq!(parsed(&[]), switch(System::Flake(None), false));
        assert_eq!(
            parsed(&["/etc/nixos"]),
            switch(System::Flake(Some("/etc/nixos".into())), false)
        );
        assert_eq!(
            parsed(&["--toplevel", "/nix/store/x"]),
            switch(System::Toplevel("/nix/store/x".into()), false)
        );
        // Anywhere on the line.
        assert_eq!(
            parsed(&["--allow-downgrade", "--toplevel", "/nix/store/x"]),
            switch(System::Toplevel("/nix/store/x".into()), true)
        );
        assert_eq!(
            parsed(&["--toplevel", "/nix/store/x", "--allow-downgrade"]),
            switch(System::Toplevel("/nix/store/x".into()), true)
        );
        assert_eq!(
            parsed(&["--allow-downgrade"]),
            switch(System::Flake(None), true)
        );
        assert_eq!(parsed(&["--status"]), Some(Word::Status));
        assert_eq!(parsed(&["--abort"]), Some(Word::Abort));
        assert_eq!(
            parsed(&["--revert-to", "/nix/store/p"]),
            Some(Word::RevertTo("/nix/store/p".into()))
        );
        assert_eq!(
            parsed(&["--activation-stopped", "/nix/store/n"]),
            Some(Word::ActivationStopped("/nix/store/n".into()))
        );
        for words in [
            &["--toplevel"][..],
            &["--toplevel", ""],
            &["--revert-to"],
            &["--status", "now"],
            &["--abort", "/etc/nixos"],
            &["-f"],
            &["--force"],
            &["a", "b"],
            &[""],
        ] {
            assert_eq!(parsed(words), None, "{words:?}");
        }
    }

    #[test]
    fn a_checkouts_owner_is_named_by_their_uid() {
        let passwd = "root:x:0:0::/root:/bin/sh\nsam:x:1000:100::/home/sam:/bin/sh\nbin:x:100:1000::/:/bin/sh\n";
        assert_eq!(named(passwd, 1000).as_deref(), Some("sam"));
        assert_eq!(named(passwd, 100).as_deref(), Some("bin"));
        assert_eq!(named(passwd, 0).as_deref(), Some("root"));
        assert_eq!(named(passwd, 10), None);
        assert_eq!(named("broken\n", 0), None);
    }
}
