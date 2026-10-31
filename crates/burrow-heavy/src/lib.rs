//! heavy: one machine-wide set of slots for heavy work (a gate, a VM test, a
//! replay, a large build), shared by every repo, runner and account.
//!
//!   heavy [--wait SECONDS] -- COMMAND...  |  heavy status
//!
//! COMMAND runs in the first free slot, and waits for one while all are
//! busy (up to `--wait` seconds, then exit 75). It gets the slot's share of
//! the threads in HEAVY_JOBS, and CARGO_BUILD_JOBS and RUST_TEST_THREADS from
//! it unless they're set already. Its exit status passes through. Inside a
//! slot (HEAVY_SLOT set) heavy just runs the command, so wrapped tools can
//! call each other.
//!
//! A slot is a lock file that anyone may read; flock(2) needs no more, so
//! runners with a read-only /run share them too. The command never gets the
//! slot's descriptor (it's close-on-exec), so a daemon it leaves behind
//! can't hold the slot.

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Write as _};
use std::os::unix::process::CommandExt as _;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use rustix::fs::{FlockOperation, flock};
use rustix::io::Errno;

pub mod config;
mod run;

pub use config::Config;

/// The exit status when no slot came free within `--wait`.
pub const BUSY: u8 = 75;
/// The exit status for a call heavy can't read.
pub const USAGE: u8 = 2;

const USAGE_LINE: &str = "usage: heavy [--wait SECONDS] -- COMMAND...  |  heavy status";

/// A line to stderr, best effort: heavy's own words are only ever
/// diagnostics, and the command's status is what the caller acts on.
macro_rules! note {
    ($($arg:tt)*) => {{
        let _ = ::std::io::Write::write_fmt(
            &mut ::std::io::stderr().lock(),
            format_args!("{}\n", format_args!($($arg)*)),
        );
    }};
}
pub(crate) use note;

/// What a call asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Call {
    /// Which slots are busy.
    Status,
    /// COMMAND in a slot, giving up after `wait` seconds if one is set.
    Run {
        wait: Option<u64>,
        command: Vec<OsString>,
    },
}

/// Reads a call's arguments: `status` first, or options up to `--` or the
/// first word that isn't one, then the command.
pub fn parse(args: &[OsString]) -> Option<Call> {
    if args.first().is_some_and(|a| a == "status") {
        return Some(Call::Status);
    }
    let mut wait = None;
    let mut rest = args;
    loop {
        match rest {
            [flag, value, more @ ..] if flag == "--wait" => {
                let value = value.to_str()?;
                if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                    return None;
                }
                wait = Some(value.parse().ok()?);
                rest = more;
            }
            [flag] if flag == "--wait" => return None,
            [dashes, more @ ..] if dashes == "--" => {
                rest = more;
                break;
            }
            _ => break,
        }
    }
    if rest.is_empty() {
        return None;
    }
    Some(Call::Run {
        wait,
        command: rest.to_vec(),
    })
}

/// The threads this process may run on, as nproc counts them: its CPU
/// affinity.
fn threads() -> io::Result<u32> {
    Ok(rustix::thread::sched_getaffinity(None)?.count())
}

/// Each slot's share of the threads, at least one.
fn share(threads: u32, slots: u32) -> u32 {
    (threads / slots).max(1)
}

/// The whole call, after the config: its exit status.
pub fn main(config: &Config, args: &[OsString]) -> u8 {
    let Some(call) = parse(args) else {
        note!("{USAGE_LINE}");
        return USAGE;
    };
    let threads = match threads() {
        Ok(n) => n,
        Err(e) => {
            note!("heavy: can't count the threads: {e}");
            return 1;
        }
    };
    let share = share(threads, config.slots);
    match call {
        Call::Status => status(config, share, threads),
        Call::Run { command, .. }
            if std::env::var_os("HEAVY_SLOT").is_some_and(|s| !s.is_empty()) =>
        {
            exec(&command)
        }
        Call::Run { wait, command } => take(config, share, wait, &command),
    }
}

/// Which slots are free. Trying each lock is the one test that works in
/// every PID namespace: /proc/locks may name a holder long gone.
fn status(config: &Config, share: u32, threads: u32) -> u8 {
    let mut out = format!("{} slots of {share} threads (of {threads})\n", config.slots);
    for i in 1..=config.slots {
        let state = match File::open(config.slot(i)) {
            Err(_) => "missing",
            Ok(file) => match flock(&file, FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => "free",
                Err(_) => "busy",
            },
        };
        out.push_str(&format!("slot{i}: {state}\n"));
    }
    let _ = io::stdout().lock().write_all(out.as_bytes());
    0
}

/// Inside a slot already: the command itself, in heavy's place.
fn exec(command: &[OsString]) -> u8 {
    let e = Command::new(&command[0]).args(&command[1..]).exec();
    note!("heavy: {}: {e}", command[0].display());
    not_run(&e)
}

/// The shell's status for a command that couldn't start.
pub(crate) fn not_run(e: &io::Error) -> u8 {
    if e.kind() == io::ErrorKind::NotFound {
        127
    } else {
        126
    }
}

/// Takes the first free slot and runs the command in it, waiting for one
/// while every slot is busy.
fn take(config: &Config, share: u32, wait: Option<u64>, command: &[OsString]) -> u8 {
    let deadline = wait.map(|s| Instant::now() + Duration::from_secs(s));
    let expired = || deadline.is_some_and(|d| Instant::now() >= d);
    let mut said = false;
    loop {
        match attempt(config) {
            Err(code) => return code,
            Ok(Some((i, slot))) => {
                let code = run::run(config, i, share, command);
                drop(slot);
                return code;
            }
            Ok(None) => {}
        }
        if expired() {
            note!("heavy: every slot still busy after {}s", wait.unwrap_or(0));
            return BUSY;
        }
        if !said {
            note!(
                "heavy: all {} slots busy, waiting (heavy status)",
                config.slots
            );
            said = true;
        }
        let mut nap = Duration::from_secs(1);
        if let Some(d) = deadline {
            nap = nap.min(d.saturating_duration_since(Instant::now()));
        }
        thread::sleep(nap);
        if expired() {
            note!("heavy: every slot still busy after {}s", wait.unwrap_or(0));
            return BUSY;
        }
    }
}

/// The first free slot, locked, or none; an error is the exit status.
fn attempt(config: &Config) -> Result<Option<(u32, File)>, u8> {
    for i in 1..=config.slots {
        let path = config.slot(i);
        let file = File::open(&path).map_err(|e| {
            note!("heavy: cannot open {}: {e}", path.display());
            1
        })?;
        match flock(&file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => return Ok(Some((i, file))),
            Err(Errno::WOULDBLOCK) => {}
            Err(e) => {
                note!("heavy: flock failed on {}: {e}", path.display());
                return Err(1);
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<OsString> {
        a.iter().map(OsString::from).collect()
    }

    fn run(wait: Option<u64>, command: &[&str]) -> Option<Call> {
        Some(Call::Run {
            wait,
            command: args(command),
        })
    }

    #[test]
    fn calls_read_as_the_usage_says() {
        assert_eq!(parse(&args(&["status"])), Some(Call::Status));
        assert_eq!(
            parse(&args(&["--", "make", "-j"])),
            run(None, &["make", "-j"])
        );
        assert_eq!(parse(&args(&["make"])), run(None, &["make"]));
        assert_eq!(
            parse(&args(&["--wait", "600", "--", "true"])),
            run(Some(600), &["true"])
        );
        assert_eq!(
            parse(&args(&["--wait", "0", "true"])),
            run(Some(0), &["true"])
        );
        assert_eq!(
            parse(&args(&["--wait", "1", "--wait", "2", "--", "x"])),
            run(Some(2), &["x"])
        );
        // After `--`, everything is the command's, `status` and `--wait` too.
        assert_eq!(parse(&args(&["--", "status"])), run(None, &["status"]));
        assert_eq!(
            parse(&args(&["--", "--wait", "1"])),
            run(None, &["--wait", "1"])
        );
        assert_eq!(parse(&args(&["--", "--", "x"])), run(None, &["--", "x"]));
    }

    #[test]
    fn a_call_it_cant_read_is_refused() {
        for bad in [
            &[][..],
            &["--"],
            &["--wait"],
            &["--wait", "5"],
            &["--wait", "5", "--"],
            &["--wait", "", "--", "x"],
            &["--wait", "-1", "--", "x"],
            &["--wait", "1s", "--", "x"],
            &["--wait", "+1", "--", "x"],
        ] {
            assert_eq!(parse(&args(bad)), None, "{bad:?}");
        }
    }

    #[test]
    fn each_slot_gets_its_share_of_the_threads() {
        assert_eq!(share(16, 2), 8);
        assert_eq!(share(15, 2), 7);
        assert_eq!(share(1, 4), 1);
        assert_eq!(share(4, 1), 4);
    }
}
