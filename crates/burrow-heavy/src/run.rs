//! A command in a slot: started in a session of its own, so cancelling
//! heavy (TERM, INT, HUP) cancels all of it, shells' workers too, and never
//! the caller's group. The slot stays held until every process in the
//! session's group has stopped, and whatever is left `grace` seconds after
//! the signal gets KILL. Its stdin, stdout and stderr are the caller's.

use std::ffi::OsString;
use std::os::unix::process::ExitStatusExt as _;
use std::process::{Child, Command, ExitStatus};
use std::thread;
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, getpgid, kill_process_group, test_kill_process_group};
use signal_hook::consts::{SIGCHLD, SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::Signals;

use crate::{Config, not_run, note};

/// How often a cancelled run looks for what's left of it.
const LOOK: Duration = Duration::from_millis(100);

/// Runs `command` in slot `slot` and returns its exit status, once the
/// whole of it has stopped.
pub fn run(config: &Config, slot: u32, share: u64, command: &[OsString]) -> u8 {
    // Listening before the start: a signal that comes in between is the
    // command's to answer as soon as it has a pid. SIGCHLD wakes the wait
    // when the command exits.
    let mut signals = match Signals::new([SIGTERM, SIGINT, SIGHUP, SIGCHLD]) {
        Ok(s) => s,
        Err(e) => {
            note!("heavy: can't listen for signals: {e}");
            return 1;
        }
    };
    let mut cmd = Command::new(&config.setsid);
    cmd.args(command)
        .env("HEAVY_SLOT", slot.to_string())
        .env("HEAVY_JOBS", share.to_string());
    for var in ["CARGO_BUILD_JOBS", "RUST_TEST_THREADS"] {
        if std::env::var_os(var).is_none_or(|v| v.is_empty()) {
            cmd.env(var, share.to_string());
        }
    }
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            note!("heavy: can't run {}: {e}", config.setsid.display());
            return not_run(&e);
        }
    };
    let mut run = Running::new(child);
    let grace = Duration::from_secs(config.grace);
    let mut cancelled: Option<Instant> = None;
    let mut killed = false;
    loop {
        // Until it's cancelled, the next signal is the next thing to look
        // at; once cancelled, what's left of the command is looked for on a
        // clock, so the KILL comes on time even if nothing signals.
        let got: Vec<i32> = if cancelled.is_none() && run.exit.is_none() {
            signals.wait().collect()
        } else {
            signals.pending().collect()
        };
        if cancelled.is_none() && got.iter().any(|&s| s != SIGCHLD) {
            cancelled = Some(Instant::now() + grace);
            run.stop(Signal::TERM);
        }
        run.reap();
        match (cancelled, run.exit) {
            (None, Some(status)) => return code(status),
            (None, None) => {}
            (Some(kill_at), exit) => {
                if let Some(status) = exit
                    && !run.group_left()
                {
                    return code(status);
                }
                if !killed && Instant::now() >= kill_at {
                    run.stop(Signal::KILL);
                    killed = true;
                }
                thread::sleep(LOOK);
            }
        }
    }
}

/// The command's process: setsid's, which becomes the command itself.
struct Running {
    child: Child,
    pid: Pid,
    /// Its status, once reaped.
    exit: Option<ExitStatus>,
}

impl Running {
    fn new(child: Child) -> Running {
        let pid = Pid::from_child(&child);
        Running {
            child,
            pid,
            exit: None,
        }
    }

    /// Reaps the command if it has exited. Its pid, and so its group's id,
    /// stays its own while it's a zombie; once reaped, the group keeps the id
    /// from being reused for as long as it has a member.
    fn reap(&mut self) {
        if self.exit.is_none()
            && let Ok(Some(status)) = self.child.try_wait()
        {
            self.exit = Some(status);
        }
    }

    /// Signals the command's group. Until setsid has made it one, the
    /// process is still heavy's fork and in heavy's group: wait for the
    /// group, or for the process to have exited, so the signal neither
    /// misses the command nor reaches heavy's caller.
    fn stop(&mut self, sig: Signal) {
        loop {
            self.reap();
            if self.exit.is_some() {
                break;
            }
            match getpgid(Some(self.pid)) {
                Ok(group) if group == self.pid => break,
                Err(_) => break,
                Ok(_) => thread::sleep(Duration::from_millis(10)),
            }
        }
        let _ = kill_process_group(self.pid, sig);
    }

    /// Whether anything of the command's group is still running.
    fn group_left(&self) -> bool {
        test_kill_process_group(self.pid).is_ok()
    }
}

/// An exit status as a shell reports it: a signal is 128 plus its number.
fn code(status: ExitStatus) -> u8 {
    let code = status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0));
    u8::try_from(code).unwrap_or(u8::MAX)
}
