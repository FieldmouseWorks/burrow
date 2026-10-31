//! burrow-watchdog: work that has stopped moving gets a look before anyone
//! has to notice it (modules/watchdog.nix runs it).
//!
//! Each source (the steward's open PRs, say) keeps a watch file of what it
//! waits on right now, and by when each wait should have ended:
//!
//! ```text
//! {"at": EPOCH, "items": {KEY: {"what": "...", "link": URL or null,
//!   "phase": "...", "since": EPOCH, "due": EPOCH, "kick": "HANDLER[/OWNER/REPO]" or null}}}
//! ```
//!
//! `phase` says what the item waits on, and `since` when it started waiting
//! on that: a new phase is progress, with a deadline of its own. `at` is when
//! the source last wrote the file. One that should write it at least every
//! `stale` seconds and hasn't is stuck itself, and its items wait with it:
//! one stall, not one an item.
//!
//! A pass (burrow-events starts one when a source kicks it because its items
//! changed, when a job of its ends, and at the deadline it booked last) takes
//! each stall a step further, and no further while that step has time left:
//!
//!   1. An item that names a kick is kicked: a delivery GitHub never made,
//!      say, costs a pass, not a job. It's looked at again GRACE later.
//!   2. Still stuck: one Claude job (proj, in the machine's own project,
//!      ahead of the line) gets every stall that's ready, to find why and get
//!      it moving. Not a turn only waiting for a slot (`line`), which that job
//!      would only queue ahead of: that goes straight to step 3.
//!   3. Still stuck GRACE after that job has ended, escalateAfter after it was
//!      handed, or START after it was handed if it hasn't started (what's
//!      stuck may be the slot it waits for): it goes to the steward's "Stuck
//!      in the pipeline" issue (the steward publishes escalated.json with the
//!      rest of that issue), and that's the last step. Its job, if it still
//!      waits for a slot and no other stall needs it, is withdrawn (proj
//!      stop): it would only take a slot for what the issue has now. proj's
//!      inbox hands the issue to a job of its own, run --unstick so it never
//!      waits in proj's line, and each new entry is that job's next turn; it
//!      puts what's truly the owner's in front of them itself.
//!   4. The owner hears, one notice a stall (notify), closed once it clears,
//!      only of what the stuck issue can't take: with no steward, when the
//!      steward is what's stuck, or when it's the stuck issue's own job
//!      (`unstick`), which the issue would only hand to itself. A notice from
//!      before the stuck issue took its stall on is closed. A stall told of
//!      keeps its job, queued or not: the only Claude still on it.
//!
//! notify never says whether GitHub took a notice, so one is sent again every
//! RENOTIFY while its stall lasts (notify adds a comment at most once a day).
//! A close is sent again on every pass until `notify pending` says it got
//! through, and the stall waits beside it meanwhile: back as it was (its
//! source stale a while, say), it carries on from where it had got to, its
//! notice still open. The hourly catch-up's pass is the latest a retry waits.
//!
//! A stall that clears is otherwise forgotten, and so is all it escalated,
//! its job withdrawn the same way if it never got a slot. A
//! job that finds an item legitimately still under way (a long build, a busy
//! CI queue) gives it more time, once and with a reason:
//!
//! ```text
//! burrow-watchdog snooze SOURCE KEY MINUTES WHY
//! ```
//!
//! `burrow-watchdog status` prints what each source waits on and how far each
//! stall has gone, changing nothing.

pub mod config;
pub mod ladder;
pub mod state;
pub mod watch;
pub mod words;
pub mod world;

/// Seconds in a minute.
pub const MIN: f64 = 60.0;
/// For a kicked pass, or a job's fix, to show.
pub const GRACE: f64 = 5.0 * MIN;
/// For the watchdog's own job to get a slot.
pub const START: f64 = 15.0 * MIN;
/// A job that couldn't start is tried again this long after.
pub const RETRY: f64 = 5.0 * MIN;
/// A notice to the owner is sent again this often while its stall lasts.
pub const RENOTIFY: f64 = 60.0 * MIN;
/// The most a snooze gives, in minutes.
pub const SNOOZE_MAX: u64 = 6 * 60;
/// The source that publishes the stuck issue.
pub const STEWARD: &str = "autoland";
/// The source whose items are proj jobs.
pub const PROJ: &str = "proj";
/// What the stuck issue says of each stall on it.
pub const ESCALATED: &str = "Stalled past its deadline, and a Claude job couldn't move it";
/// What it says of a turn waiting for a slot past its deadline, which no
/// job was handed (one would only have queued too).
pub const IN_LINE: &str = "Waiting in proj's line past its deadline";

/// The time now, in seconds since the epoch, as the watch files have it.
pub fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}
