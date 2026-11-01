//! Once a day, after a backup: forget what burrow.backup.keep no longer keeps
//! and prune it, read back the next share of the repository in turn (all of
//! it every verifyDays days) and every share found damaged until it reads
//! back whole, then the drill. What it finds goes to state/problems,
//! reported on every pass until the next day's upkeep finds it gone: a new
//! backup, or another share reading back fine, clears nothing.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::process::Stdio;
use std::time::{Duration, SystemTime};

use crate::{Run, age, drill, read, touch, write};

/// How long the day's upkeep holds: under a day, so a daily backstop that
/// runs a little early still does it.
const DAY: Duration = Duration::from_secs(72_000);

pub fn maintain(run: &Run) -> Result<(), String> {
    let cfg = run.cfg;
    let state = &cfg.state;
    let maintained = state.join("maintained");
    let fresh = fs::metadata(&maintained)
        .and_then(|m| m.modified())
        .is_ok_and(|t| age(t, SystemTime::now()) < DAY);
    if fresh {
        return Ok(());
    }
    let mut problems = Vec::new();
    let pruned = run
        .restic()
        .args(["forget", "--host", &cfg.host])
        .args(cfg.keep_args())
        .args(["--prune", "-q"])
        .stdin(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !pruned {
        problems.push("restic forget --prune failed".to_owned());
    }
    let part = cursor(read(&state.join("check-next"))?.as_deref(), cfg.verify_days);
    let mut specs: BTreeSet<String> =
        damaged(read(&state.join("damaged"))?.as_deref().unwrap_or(""));
    specs.insert(format!("{part}/{}", cfg.verify_days));
    let mut still = String::new();
    for spec in &specs {
        let whole = run
            .restic()
            .args(["check", "-q", &format!("--read-data-subset={spec}")])
            .stdin(Stdio::null())
            .stdout(io::stderr())
            .status()
            .is_ok_and(|s| s.success());
        if !whole {
            still.push_str(&format!("{spec}\n"));
            problems.push(format!(
                "restic check found damage reading part {spec} of the repository back"
            ));
        }
    }
    write(&state.join("damaged"), still.as_bytes())?;
    if !drill::drill(run, &cfg.drill) {
        problems.push("the restore drill failed".to_owned());
    }
    let listed: String = problems.iter().map(|p| format!("{p}\n")).collect();
    write(&state.join("problems"), listed.as_bytes())?;
    // Last, so an upkeep cut short is done again in full, on the same share.
    write(
        &state.join("check-next"),
        format!("{}\n", part % cfg.verify_days + 1).as_bytes(),
    )?;
    touch(&maintained)
}

/// The day's share, from the cursor kept: one that isn't a whole number
/// (cut short, say) starts over at the first, and one from a bigger
/// verifyDays folds into range.
pub fn cursor(kept: Option<&str>, days: u32) -> u32 {
    let part = kept
        .filter(|k| (1..=9).contains(&k.len()) && k.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|k| k.parse::<u32>().ok())
        .filter(|&p| p >= 1)
        .unwrap_or(1);
    (part - 1) % days + 1
}

/// The shares found damaged and not yet read back whole, as N/T: only the
/// ones restic takes (1 <= N <= T <= 256), whatever verifyDays was when they
/// were found.
pub fn damaged(kept: &str) -> BTreeSet<String> {
    let number = |s: &str| (1..=3).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit());
    kept.lines()
        .filter_map(|l| {
            let (n, t) = l.split_once('/')?;
            if !number(n) || !number(t) {
                return None;
            }
            let (n, t): (u32, u32) = (n.parse().ok()?, t.parse().ok()?);
            (1 <= n && n <= t && t <= 256).then(|| format!("{n}/{t}"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cursor_starts_over_when_it_doesnt_read() {
        for (kept, days, part) in [
            (None, 30, 1),
            (Some(""), 30, 1),
            (Some("1"), 30, 1),
            (Some("7"), 30, 7),
            (Some("30"), 30, 30),
            (Some("31"), 30, 1),
            (Some("45"), 30, 15),
            (Some("0"), 30, 1),
            (Some("007"), 30, 7),
            (Some("x"), 30, 1),
            (Some("2 "), 30, 1),
            (Some("1\n2"), 30, 1),
            (Some("1234567890"), 30, 1),
            (Some("123456789"), 2, 1),
            (Some("2"), 1, 1),
        ] {
            assert_eq!(cursor(kept, days), part, "{kept:?} of {days}");
        }
    }

    #[test]
    fn only_shares_restic_takes_stay_damaged() {
        let got: Vec<String> = damaged(
            "2/2\n0/2\n3/2\n1/256\n1/257\n007/030\nx/2\n1/2/3\n1/\n/2\n2/2\n1234/5000\n1/1",
        )
        .into_iter()
        .collect();
        assert_eq!(got, ["1/1", "1/256", "2/2", "7/30"]);
    }
}
