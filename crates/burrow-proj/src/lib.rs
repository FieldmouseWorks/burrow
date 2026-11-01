//! proj: hand project work to a Claude running inside that project, as a
//! background job, and check on it later. This is how the operator session
//! (the one you talk to about the machine) does project work without leaving
//! its own chat. modules/proj.nix runs it, PROJ_CONFIG naming its config.
//!
//! ```text
//! proj run PROJECT "task"     start a job; prints its id
//! proj run --vm PROJECT "task"  the same, in a fresh agent sandbox (burrow-agent)
//! proj run --first PROJECT "task"  ahead of the line (someone is waiting on it)
//! proj run --pipeline PROJECT "task"  work on the pipeline itself: in one of
//!                             its own repos (pipelineRepos, by the clone's
//!                             GitHub origin), ahead of everything, and its
//!                             own slots, and in a lead one (leadRepos) ahead
//!                             of the other pipeline work too; in any other,
//!                             as a steward job, ahead of the line within its
//!                             project's cap, and status and the turn's
//!                             journal say so (every turn of it, either way)
//! proj run --unstick PROJECT "task"  what the pipeline is stuck on: never in
//!                             the line, which may be the very thing stuck, but
//!                             in a slot of its own besides maxJobs (every turn
//!                             of it)
//! proj run --kick NAME PROJECT "task"  kick NAME (HANDLER or HANDLER/OWNER/REPO,
//!                             one of burrow-events') each time a turn starts
//!                             (takes its slot) and each time one ends
//! proj run --timeout DUR PROJECT "task"  each turn stopped after DUR (as
//!                             timeout(1) reads it: 90m, 2h) instead of timeout
//! proj run --idle DUR PROJECT "task"  each turn stopped once its stream has
//!                             said nothing for DUR: it's stuck
//! proj run --owned --kick NAME PROJECT "task"  NAME acts on each turn's end
//!                             itself (the steward, the watchdog): the turn
//!                             needn't say what comes next
//! proj run --link URL PROJECT "task"  what the job is about (its issue);
//!                             empty for nothing
//! proj run --app NAME PROJECT "task"  the job acts on GitHub as the App NAME,
//!                             one of burrow.github.apps, never as the user
//! proj run --key KEY PROJECT "task"  what its starter knows it by before
//!                             it has its id: one job a key, taken at once
//!                             (a key a started job has is refused)
//! proj reply [--first] ID "message"  continue that job's conversation
//! proj reply --cut-off N ID "message"  so, if its turn N was cut off
//!                             (else exit 3)
//! proj status                 recent jobs and their state
//! proj status --line          every turn in line, next first, and its wait
//! proj log ID [--turn N]      the job's latest answer, or turn N's (--raw for
//!                             the full stream); fails when there's none
//! proj state ID               queued, running, done, error or failed
//! proj find KEY               the id of the job run --key KEY, once it has
//!                             started a turn; nothing for none, and one
//!                             that never started lets its key go
//! proj stop ID                end a job: its running turn and its waits
//! proj withdraw ID            end a job only while its turn still waits for
//!                             a slot: prints withdrawn, or running or over
//!                             for one it leaves be
//! proj unstick ID             make a job one run --unstick from now on: a
//!                             turn of it waiting in line leaves it at its
//!                             next look, one running carries on
//! proj list                   projects you can run jobs in
//! proj await TARGET...        from inside a job: resume it once a PR or
//!                             issue (URL or OWNER/REPO#N) changes; one
//!                             already merged or closed is refused
//! proj ask                    from inside a job: its answer is a question for
//!                             a person, whose reply resumes it
//! proj done                   from inside a job: its work is finished
//! ```
//!
//! Jobs run headless (claude -p) in their project under the same managed
//! settings as every session: no prompts, and an R2 command is refused rather
//! than asked, so the job reports it and the operator brings it to the owner.
//! Everything a job said is kept in its folder in jobs ([`job`] has what's
//! in it); what it builds goes in a scratch dir of its own, PROJ_SCRATCH,
//! cleared once the job is over ([`scratch`]).
//!
//! At most maxJobs turns run Claude at once, and a turn starts only with
//! minFreeMemory MiB to spare; the rest wait in line (queued), lead jobs'
//! first, then the other pipeline jobs', then --first ones, then oldest first
//! (a job run --since dates from then, every turn of it), and a turn that has waited maxWait seconds counts as --first itself, so a
//! stream of them can't starve it. pipelineSlots of the slots are for pipeline
//! jobs in the pipeline's own repos alone, and no project's other jobs hold
//! more than maxJobsPerProject at once, the last of those kept for its steward
//! jobs, and a waiting one of theirs is owed a
//! slot ahead of the pipeline's when none runs ([`pick`]). A job run --unstick
//! waits in none of that: its turns take a slot of their own, past maxJobs, one at a
//! time ([`slot`]). A waiting turn becomes the deployed proj at its next look
//! after a switch ([`slot`]). Every turn runs in the
//! slice, which the machine caps, so jobs can't take memory from anything
//! else, and gets its timeout before it's stopped, so a stuck one can't hold
//! its slot for good. A job's --idle stops a stuck one sooner: a turn at work
//! says something at least each time Claude's Bash tool stops a command (ten
//! minutes at most), so one whose stream has been quiet longer than that is
//! stuck, however long the work itself may take. Waiting its turn isn't being
//! stuck: a command that waits in line inside a turn for something shared (a
//! machine-wide lock) says so in the turn's PROJ_WAIT_DIR ([`waits::waiting`]
//! has how), and the turn's timeout stands still meanwhile, the stall watchdog
//! holding that wait to stallAfterMinutes instead.
//!
//! A run killed for memory before it answers (Claude, as a rule) is carried on
//! in its own conversation and slot, twice a turn at most ([`turn`]).
//!
//! A turn ends at its answer. Claude -p stays up after it while anything it
//! left in the background runs (a CI watch, say), holding a slot to do
//! nothing; so the answer stops it, and what it left running. That can be work
//! it was waiting on, a build longer than one Bash call: once a turn, it hears
//! it was stopped and answers again, in the same slot. A job waiting on GitHub
//! says so with `proj await`, and `proj _watch` gives it its next turn once
//! what it waits on changes: checks finish, a review, comment or push comes
//! in, or it's merged or closed. Nothing changed in awaitHours, and it's told
//! that instead. burrow-events starts _watch on the repo's webhook deliveries,
//! when a turn that awaits it ends, and at the deadline _watch books in its
//! wake file.
//!
//! Nothing else resumes a job, so every turn says what comes next, or it
//! would wait for good on something nobody will tell it about (a background
//! run, a notification): `proj await`, `proj ask` or `proj done`, before it
//! answers. One that answers without saying hears so once, in the same slot,
//! and answers again; still silent, it's a stall. Only a job whose starter
//! acts on each turn's end itself (--owned: the steward's, the watchdog's)
//! needn't say. With the stall watchdog on, proj is one of its sources: its
//! watch file has each job's wait ([`waits`]) as of its last change, and the
//! watchdog is kicked whenever that changes.
//!
//! With --vm the job runs in its own microVM on a clone of the project, where
//! nothing is refused at all; the work comes back with `agent fetch` (the
//! sandbox id is in the job's `agent` and in `proj status`).
//!
//! With --app every turn runs in that App's env (modules/github.nix), its
//! file from apps, so git and gh act as the App: the machine's own work, not
//! the user's. A turn whose App is gone fails rather than run as the user.

use std::fs;
use std::io;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub mod cmd;
pub mod config;
pub mod github;
pub mod inbox;
pub mod job;
pub mod pick;
pub mod scratch;
pub mod slot;
pub mod systemd;
pub mod turn;
pub mod waits;
pub mod watch;

/// Seconds the stall watchdog gives proj to have acted on a deadline of its
/// own before it does.
pub const GRACE: i64 = 600;
/// How long a failed turn has to be resumed before it's a stall: proj-inbox
/// resumes an issue's job seconds later when a reply waits.
pub const RESUME_GRACE: i64 = 120;

/// How a command ends when it doesn't succeed: its exit status, and what it
/// says on stderr, if anything.
#[derive(Debug, PartialEq)]
pub struct Fail {
    pub code: u8,
    pub say: Option<String>,
}

impl Fail {
    pub fn new(code: u8, say: impl Into<String>) -> Fail {
        Fail {
            code,
            say: Some(say.into()),
        }
    }

    /// An exit status with nothing more to say.
    pub fn quiet(code: u8) -> Fail {
        Fail { code, say: None }
    }
}

/// What can go wrong reading or writing a file, said with its path.
pub fn io_fail(path: &Path, e: io::Error) -> Fail {
    Fail::new(1, format!("proj: {}: {e}", path.display()))
}

/// A file's text as `$(cat FILE)` has it, its trailing newlines dropped; None
/// when it can't be read.
pub fn read(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    Some(
        String::from_utf8_lossy(&bytes)
            .trim_end_matches('\n')
            .to_owned(),
    )
}

/// Whether a file is there and isn't empty.
pub fn filled(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|m| m.len() > 0)
}

/// Swapped in whole, through PATH.new, so a reader never finds it missing or
/// half written.
pub fn swap(path: &Path, text: &str) -> io::Result<()> {
    let new = path.with_file_name(format!(
        "{}.new",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    fs::write(&new, text)?;
    fs::rename(&new, path)
}

/// When a file last changed, in nanoseconds since the epoch.
pub fn mtime_nanos(path: &Path) -> Option<i64> {
    let t = fs::metadata(path).ok()?.modified().ok()?;
    Some(t.duration_since(UNIX_EPOCH).ok()?.as_nanos() as i64)
}

/// When a file last changed, in whole seconds since the epoch.
pub fn mtime(path: &Path) -> Option<i64> {
    mtime_nanos(path).map(|t| t.div_euclid(1_000_000_000))
}

/// The time now, in nanoseconds since the epoch.
pub fn now_nanos() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

/// The time now, in whole seconds since the epoch.
pub fn now() -> i64 {
    now_nanos().div_euclid(1_000_000_000)
}

/// A duration as timeout(1) reads it (90, 1.5h, 0.1s, 1d; 0 for none), in
/// nanoseconds, rounded up; None for anything else. Past about 30 years it's
/// held there, as timeout(1) holds one too long for it: neither ever comes.
pub fn nanos(d: &str) -> Option<i64> {
    let (number, per) = match d.as_bytes().last()? {
        b's' => (&d[..d.len() - 1], 1.0),
        b'm' => (&d[..d.len() - 1], 60.0),
        b'h' => (&d[..d.len() - 1], 3600.0),
        b'd' => (&d[..d.len() - 1], 86400.0),
        _ => (d, 1.0),
    };
    // Digits with a point anywhere but alone, then an exponent, if any.
    let (mantissa, exponent) = match number.find(['e', 'E']) {
        Some(i) => (&number[..i], Some(&number[i + 1..])),
        None => (number, None),
    };
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if !digits(whole) || !digits(fraction) || whole.is_empty() && fraction.is_empty() {
        return None;
    }
    if let Some(e) = exponent {
        let e = e.strip_prefix(['-', '+']).unwrap_or(e);
        if e.is_empty() || !digits(e) {
            return None;
        }
    }
    let s = number.parse::<f64>().ok()? * per * 1e9;
    Some(s.ceil().min(1e18) as i64)
}

#[cfg(test)]
mod tests {
    use super::nanos;

    // What timeout(1) takes, to the nanosecond and rounded up; what it
    // doesn't, none.
    #[test]
    fn durations() {
        assert_eq!(nanos("90"), Some(90_000_000_000));
        assert_eq!(nanos("1.5h"), Some(5_400_000_000_000));
        assert_eq!(nanos("0.1s"), Some(100_000_000));
        assert_eq!(nanos("1d"), Some(86_400_000_000_000));
        assert_eq!(nanos("2m"), Some(120_000_000_000));
        assert_eq!(nanos(".5"), Some(500_000_000));
        assert_eq!(nanos("1."), Some(1_000_000_000));
        assert_eq!(nanos("1e3"), Some(1_000_000_000_000));
        assert_eq!(nanos("1E-9s"), Some(1));
        assert_eq!(nanos("0.0000000001"), Some(1));
        assert_eq!(nanos("0"), Some(0));
        assert_eq!(nanos("1e30"), Some(1_000_000_000_000_000_000));
        for bad in [
            "", "s", ".", "-1", "1x", "1.2.3", "1e", "e3", "1 h", "soon", "1hh",
        ] {
            assert_eq!(nanos(bad), None, "{bad}");
        }
    }
}
