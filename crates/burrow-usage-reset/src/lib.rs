//! burrow-usage-reset: the Claude account's banked weekly reset, used once,
//! when the week's usage is nearly gone. Each pass reads the weekly meter
//! through Claude Code (logged in to the owner's plan as the machine's user),
//! runs Claude Code's own /limit-reset if the week has less than its line
//! left and a reset is banked, tells the owner it did, and books its next
//! look by how fast the week is going. Nothing here runs on a timer:
//! burrow-events starts each pass at the time the last one booked
//! (modules/usage-reset.nix).

pub mod claude;
pub mod pass;

use std::time::{SystemTime, UNIX_EPOCH};

/// The time now, in seconds since the epoch, as the wake file has it.
pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}
