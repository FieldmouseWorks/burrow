//! Provider-agnostic credit meter queries backed by `burrow-probe::credit`.
//!
//! Used by routing (`engine/route.rs`) and drain admission (`slot/room.rs`,
//! `inbox/drain.rs`) to check room, threshold spend, and next opening time for
//! any engine metered by `proj.apiCredit`, without depending on any provider
//! crate. Atomic run reservation lives inside the engine plugin itself.

use std::fs::File;
use std::io::Read;
use std::path::Path;

/// Whether a configured credential file exists, can be read, and is non-empty.
/// Never includes file contents in any diagnostic.
pub fn has_key(key_file: Option<&Path>) -> bool {
    let Some(path) = key_file else {
        return false;
    };
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let mut buf = [0u8; 256];
    loop {
        match file.read(&mut buf) {
            Ok(0) => return false,
            Ok(n) if buf[..n].iter().any(|b| !b.is_ascii_whitespace()) => return true,
            Ok(_) => continue,
            Err(_) => return false,
        }
    }
}

/// Whether the credit meter has room now (observer check; no lock taken).
pub fn room(credit_file: Option<&Path>, jobs: &Path, now: i64) -> bool {
    credit_file.is_some_and(|credit| {
        burrow_probe::credit::meter(credit, Some(jobs), now).is_ok_and(|m| m.room())
    })
}

/// Whether the credit meter is short at `pct` percent of its cycle budget, or
/// has no room / cannot be read.
pub fn short(credit_file: Option<&Path>, jobs: &Path, pct: f64, now: i64) -> bool {
    let Some(credit) = credit_file else {
        return true;
    };
    burrow_probe::credit::meter(credit, Some(jobs), now).map_or(true, |m| {
        !m.room() || m.spent as f64 * 100.0 >= pct * m.cycle.budget as f64
    })
}

/// When the clock alone may give the credit meter room again.
pub fn opens(credit_file: Option<&Path>, jobs: &Path, now: i64) -> Option<i64> {
    let credit = credit_file?;
    burrow_probe::credit::meter(credit, Some(jobs), now)
        .ok()?
        .opens()
}
