//! burrow-dispatch: what every PR waits on together keeps moving
//! (modules/dispatch.nix runs it).
//!
//! The steward sees to each PR, and the stall watchdog to each wait that has
//! gone on past its deadline. What neither owns is what every PR waits on at
//! once: the machine's Actions runners, and its deploy. A pass takes one
//! snapshot of them (snapshot.rs), decides (rules.rs, which only reads it)
//! and makes the moves its rules call for, from a short, fixed list. Each is
//! looked at again just before it's made, since the snapshot may have moved
//! on while the pass read the rest, and isn't made if it has or can't be
//! read; it's kept before it's made, so a pass cut short still counts it,
//! and logged:
//!
//!   - A runner with jobs queued that it could take, idle (no Runner.Worker
//!     in its unit) for `quiet`, has lost GitHub, and is restarted; one that
//!     has failed is started. Never while a switch is in flight, which
//!     restarts what it changes itself, and at most RESTARTS times in
//!     RECENT: past that, restarting isn't the fix.
//!   - A push run on a runner repo's default branch that no job of has
//!     started, with a later run of its workflow on that branch live, is
//!     cancelled, so the later one takes its place on the runner. One that
//!     has started finishes, so the branch always gets a result, and one
//!     someone ran again is theirs. Each run is tried once in RECENT.
//!   - A job queued for the fresh runners, more than they have idle, gets
//!     one: registered for that one job (GitHub's JIT config) and started
//!     as a unit of its own, whose user and state nothing ran as before,
//!     up to `most` at once. One that sits idle for `quiet` won't take a
//!     job, and is stopped (fresh.rs).
//!
//! What it waits on goes in its watch file, a stall watchdog source, so a
//! move that didn't help is a stall like any other, and the watchdog's
//! ladder (a kick, a Claude job, the stuck issue, the owner) takes it on:
//!
//!   - runner/UNIT: jobs queued for an idle runner, from the pass that first
//!     saw it so, due once its restarts, and the looks after them, have had
//!     their time, kicking a pass for a fresh look first. It links the
//!     oldest job and never names it: a job's name is its workflow's, which
//!     anyone whose PR runs one writes, and the watchdog's jobs read this.
//!   - fresh: jobs queued for the fresh runners while there's room for
//!     more, from the pass that first saw it so, due once a start and the
//!     looks after it have had their time twice over.
//!   - deploy/OWNER/REPO: commits on a watched repo's default branch that
//!     the running system wasn't built from, from the oldest, due
//!     `deploy.after` later, kicking HOST-update; while a run that started
//!     after that commit is at work, from its start, due `deploy.run` later.
//!   - github/OWNER/REPO: a repo its passes can't read from GitHub.
//!
//! burrow-events starts a pass on a runner repo's workflow_job events, a
//! push to a watched repo's default branch, HOST-update starting and ending,
//! and the wake-up the last pass booked, no closer together than its
//! handler's gap (modules/dispatch.nix). `burrow-dispatch status` shows what
//! a pass would see and do, and what passes have done, changing nothing.

pub mod config;
pub mod fresh;
pub mod pass;
pub mod rules;
pub mod snapshot;
pub mod state;
pub mod world;
