//! A finished job's scratch, once proj has set it aside: the names it's set
//! aside under, and the one owner of removing it. proj renames SCRATCH/jobs/ID
//! to [`GONE`]ID-... ([`aside`]) and never looks inside it again; a path unit
//! in modules/proj.nix starts `burrow-scratch sweep` as root whenever a name
//! with that prefix exists ([`sweep`]), and an entry it can't remove is renamed
//! to [`STUCK`]..., out of the watched name, and noticed once ([`notice`]).
//!
//! These constants are the one spelling of the names: proj takes them from
//! here, and modules/proj.nix reads [`GONE`] out of this file for its glob, so
//! evaluation fails if the line moves.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub mod notice;
pub mod sweep;

pub use notice::notice;
pub use sweep::{Sweep, sweep};

/// What proj renames a finished job's scratch to start with: the name the path
/// unit watches.
pub const GONE: &str = ".gone-";

/// What an entry the sweep couldn't remove is renamed to start with: out of
/// the watched name, tried again on each later sweep.
pub const STUCK: &str = ".stuck-";

/// Linux `NAME_MAX`: every path component `burrow-scratch` constructs fits
/// within it so renames never fail with `ENAMETOOLONG`.
pub const NAME_MAX: usize = 255;

/// A name to set job `id`'s scratch aside under, one no other clear takes:
/// two at once can't take each other's, and one set aside earlier that the
/// sweep hasn't reached yet (or couldn't remove) never stands in the way.
pub fn aside(id: &str) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let n = SET_ASIDE.fetch_add(1, Ordering::Relaxed);
    let suffix = format!("-{}-{now}-{n}", std::process::id());
    let max_id = NAME_MAX.saturating_sub(GONE.len() + suffix.len());
    let mut end = max_id.min(id.len());
    while !id.is_char_boundary(end) {
        end -= 1;
    }
    format!("{GONE}{}{suffix}", &id[..end])
}

/// How many this process has set aside.
static SET_ASIDE: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_aside_names_are_the_watched_ones() {
        let (a, b) = (aside("j1"), aside("j1"));
        assert!(a.starts_with(".gone-j1-") && a.starts_with(GONE), "{a}");
        assert_ne!(a, b);
        assert!(!STUCK.starts_with(GONE) && !GONE.starts_with(STUCK));
    }

    #[test]
    fn long_ids_stay_within_name_max() {
        let long = "a".repeat(255);
        let (a, b) = (aside(&long), aside(&long));
        assert!(a.starts_with(GONE) && a.len() <= 255, "len={}", a.len());
        assert!(b.starts_with(GONE) && b.len() <= 255, "len={}", b.len());
        assert_ne!(a, b);

        let utf8 = "é".repeat(127);
        let u = aside(&utf8);
        assert!(u.starts_with(GONE) && u.len() <= 255, "len={}", u.len());

        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join(&long);
        std::fs::create_dir(&src).unwrap();
        std::fs::rename(&src, dir.path().join(&a)).unwrap();
    }
}
