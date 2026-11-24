//! heavy: one machine-wide set of slots for heavy work (a gate, a VM test, a
//! replay, a large build), shared by every repo, runner and account.
//!
//!   heavy [--wait SECONDS] [--first | --all] -- COMMAND...  |  heavy status
//!
//! COMMAND runs in the first free slot, and waits in line for one while all
//! are busy or others wait ahead of it (up to `--wait` seconds, then exit
//! 75): a slot freed is the longest waiter's (line.rs), `--first`'s before
//! anyone else's, which is for what the machine's own deploy waits on
//! (HOST-update's VM tests). It gets the slot's share of
//! the threads in HEAVY_JOBS, and CARGO_BUILD_JOBS and RUST_TEST_THREADS from
//! it unless they're set already. NIX_CONFIG defaults to max-jobs = 1 and
//! cores = the share, before any caller settings so those win. These bound
//! Nix's per-client build concurrency and advertised cores, not a CPU quota.
//! With `--all` it takes every slot instead,
//! and gets all of the threads: for a run that has to have the machine to
//! itself, such as a long gate with a deadline. While one waits, no
//! ordinary run starts, so the machine empties under it. Its exit status passes through. Inside a
//! slot (HEAVY_SLOT set) heavy just runs the command, so wrapped tools can
//! call each other.
//!
//! A slot is a lock file that anyone may read; flock(2) needs no more, so
//! runners with a read-only /run share them too. The command never gets the
//! slot's descriptor (it's close-on-exec), so a daemon it leaves behind
//! can't hold the slot.

use std::cell::Cell;
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Write as _};
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use rustix::fs::{FlockOperation, flock};
use rustix::io::Errno;
use wait::Waiting;

pub mod config;
pub mod cpus;
mod line;
mod run;
mod wait;

pub use config::Config;
pub use line::Class;
use line::Place;

/// The exit status when no slot came free within `--wait`.
pub const BUSY: u8 = 75;
/// The exit status for a call heavy can't read.
pub const USAGE: u8 = 2;

const USAGE_LINE: &str =
    "usage: heavy [--wait SECONDS] [--first | --all] -- COMMAND...  |  heavy status";

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
    /// COMMAND in a slot, or in every slot as `Class::All`, after whoever
    /// is ahead of its class in the line, giving up after `wait` seconds if
    /// one is set.
    Run {
        wait: Option<u64>,
        class: Class,
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
    let mut class = Class::Ordinary;
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
            [flag, more @ ..] if flag == "--all" || flag == "--first" => {
                let asked = if flag == "--all" {
                    Class::All
                } else {
                    Class::First
                };
                if class != Class::Ordinary && class != asked {
                    return None;
                }
                class = asked;
                rest = more;
            }
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
        class,
        command: rest.to_vec(),
    })
}

/// Each slot's share of the threads, at least one.
fn share(threads: u64, slots: u32) -> u64 {
    (threads / u64::from(slots)).max(1)
}

/// The whole call, after the config: its exit status.
pub fn main(config: &Config, args: &[OsString]) -> u8 {
    let Some(call) = parse(args) else {
        note!("{USAGE_LINE}");
        return USAGE;
    };
    let threads = match cpus::threads() {
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
        Call::Run {
            wait,
            class: Class::All,
            command,
        } => take_all(config, threads, wait, &command),
        Call::Run {
            wait,
            class,
            command,
        } => take(config, share, class, wait, &command),
    }
}

/// Which slots are free. Trying each lock is the one test that works in
/// every PID namespace: /proc/locks may name a holder long gone.
fn status(config: &Config, share: u64, threads: u64) -> u8 {
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

/// When a wait runs out, and whether it has slept yet: a try comes after
/// the deadline only while it hasn't, so `--wait 0` still tries every lock
/// once, and once a run has waited the deadline holds for all of it.
struct Clock {
    deadline: Option<Instant>,
    slept: bool,
}

impl Clock {
    fn new(wait: Option<u64>) -> Clock {
        Clock {
            deadline: wait.map(|s| Instant::now() + Duration::from_secs(s)),
            slept: false,
        }
    }

    fn expired(&self) -> bool {
        self.deadline.is_some_and(|d| Instant::now() >= d)
    }

    /// Tries `get` until it gives something, napping up to a second between
    /// tries and saying `waiting` once; none once the wait has run out.
    fn poll<T>(
        &mut self,
        said: &mut bool,
        waiting: impl Fn() -> String,
        mut get: impl FnMut() -> Result<Option<T>, u8>,
    ) -> Result<Option<T>, u8> {
        loop {
            if self.slept && self.expired() {
                return Ok(None);
            }
            if let Some(got) = get()? {
                return Ok(Some(got));
            }
            if self.expired() {
                return Ok(None);
            }
            if !*said {
                note!("{}", waiting());
                *said = true;
            }
            let mut nap = Duration::from_secs(1);
            if let Some(d) = self.deadline {
                nap = nap.min(d.saturating_duration_since(Instant::now()));
            }
            thread::sleep(nap);
            self.slept = true;
        }
    }
}

/// Takes the first free slot and runs the command in it, waiting in line
/// for one while every slot is busy, while anyone is ahead, or while an
/// `--all` run has the turn. The place goes once it has the slot.
fn take(config: &Config, share: u64, class: Class, wait: Option<u64>, command: &[OsString]) -> u8 {
    let place = match Place::take(&config.dir, class) {
        Ok(place) => place,
        Err(code) => return code,
    };
    let mut clock = Clock::new(wait);
    let mut said = false;
    let line = Waiting::start("heavy: a slot, in line (heavy status)");
    // Whether the last look found someone ahead, rather than no slot free.
    let behind = Cell::new(false);
    let waiting = || {
        if behind.get() {
            "heavy: others are ahead in line, waiting (heavy status)".to_owned()
        } else {
            format!(
                "heavy: all {} slots busy, waiting (heavy status)",
                config.slots
            )
        }
    };
    let got = clock.poll(&mut said, waiting, || {
        behind.set(!place.is_next()?);
        if behind.get() || all_waiting(config)? {
            return Ok(None);
        }
        attempt(config)
    });
    drop(place);
    drop(line);
    match got {
        Err(code) => code,
        Ok(Some((i, slot))) => {
            let code = run::run(config, &i.to_string(), share, command);
            drop(slot);
            code
        }
        Ok(None) => {
            let waited = wait.unwrap_or(0);
            if behind.get() {
                note!("heavy: others still ahead in line after {waited}s");
            } else {
                note!("heavy: every slot still busy after {waited}s");
            }
            BUSY
        }
    }
}

/// Takes every slot, lowest first, and runs the command in all of them
/// with all of the threads. It first takes the turn (the `all` file), at
/// the head of the line and after any `--all` that came before it, which
/// ordinary runs look at before they take a slot: while it's held, none
/// starts, so the machine empties under the run instead of new arrivals
/// taking each slot as it comes free. Holding the turn throughout also
/// keeps two of these from waiting on each other's slots.
fn take_all(config: &Config, threads: u64, wait: Option<u64>, command: &[OsString]) -> u8 {
    let place = match Place::take(&config.dir, Class::All) {
        Ok(place) => place,
        Err(code) => return code,
    };
    let mut clock = Clock::new(wait);
    let mut said = false;
    let line = Waiting::start("heavy: all slots, in line (heavy status)");
    let waiting = || {
        format!(
            "heavy: waiting for all {} slots (heavy status)",
            config.slots
        )
    };
    let next = || {
        if place.is_next()? {
            lock(&config.all())
        } else {
            Ok(None)
        }
    };
    let turn = match clock.poll(&mut said, waiting, next) {
        Err(code) => return code,
        Ok(Some(turn)) => turn,
        Ok(None) => {
            note!(
                "heavy: another run still waiting for every slot after {}s",
                wait.unwrap_or(0)
            );
            return BUSY;
        }
    };
    // The turn holds the rest off from here on.
    drop(place);
    let mut held = Vec::new();
    for i in 1..=config.slots {
        match clock.poll(&mut said, waiting, || lock(&config.slot(i))) {
            Err(code) => return code,
            Ok(Some(slot)) => held.push(slot),
            Ok(None) => {
                note!("heavy: slot{i} still busy after {}s", wait.unwrap_or(0));
                return BUSY;
            }
        }
    }
    drop(line);
    let code = run::run(config, "all", threads, command);
    drop(held);
    drop(turn);
    code
}

/// Whether an `--all` run holds the turn: a shared lock it can't have.
/// Without the turn's file there's no turn to hold, since `--all` can't
/// run without it either, so an ordinary run doesn't need it: not while a
/// switch that adds it is still under way, nor where a caller's own slots
/// are only the slot files.
fn all_waiting(config: &Config) -> Result<bool, u8> {
    let path = config.all();
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => {
            note!("heavy: cannot open {}: {e}", path.display());
            return Err(1);
        }
    };
    match flock(&file, FlockOperation::NonBlockingLockShared) {
        Ok(()) => Ok(false),
        Err(Errno::WOULDBLOCK) => Ok(true),
        Err(e) => {
            note!("heavy: flock failed on {}: {e}", path.display());
            Err(1)
        }
    }
}

/// The first free slot, locked, or none; an error is the exit status.
fn attempt(config: &Config) -> Result<Option<(u32, File)>, u8> {
    for i in 1..=config.slots {
        if let Some(file) = lock(&config.slot(i))? {
            return Ok(Some((i, file)));
        }
    }
    Ok(None)
}

/// A lock file, or the slots' directory, opened read-only: anyone may lock
/// what they can read.
fn open(path: &Path) -> Result<File, u8> {
    File::open(path).map_err(|e| {
        note!("heavy: cannot open {}: {e}", path.display());
        1
    })
}

/// The lock file at `path`, locked, or none while someone else holds it;
/// an error is the exit status.
fn lock(path: &Path) -> Result<Option<File>, u8> {
    let file = open(path)?;
    match flock(&file, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(Some(file)),
        Err(Errno::WOULDBLOCK) => Ok(None),
        Err(e) => {
            note!("heavy: flock failed on {}: {e}", path.display());
            Err(1)
        }
    }
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
            class: Class::Ordinary,
            command: args(command),
        })
    }

    fn run_all(wait: Option<u64>, command: &[&str]) -> Option<Call> {
        Some(Call::Run {
            wait,
            class: Class::All,
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
        assert_eq!(parse(&args(&["--all", "--", "x"])), run_all(None, &["x"]));
        assert_eq!(
            parse(&args(&["--wait", "5", "--all", "x"])),
            run_all(Some(5), &["x"])
        );
        assert_eq!(
            parse(&args(&["--all", "--wait", "5", "--", "x"])),
            run_all(Some(5), &["x"])
        );
        assert_eq!(parse(&args(&["--", "--all"])), run(None, &["--all"]));
        assert_eq!(
            parse(&args(&["--first", "--wait", "5", "--", "x"])),
            Some(Call::Run {
                wait: Some(5),
                class: Class::First,
                command: args(&["x"]),
            })
        );
        assert_eq!(parse(&args(&["--", "--first"])), run(None, &["--first"]));
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
            &["--all"],
            &["--all", "--"],
            &["--first"],
            &["--first", "--all", "x"],
            &["--all", "--first", "x"],
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
