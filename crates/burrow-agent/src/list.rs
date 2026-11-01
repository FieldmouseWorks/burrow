//! `agent ls` and `agent gc`: what each sandbox is doing, and what the
//! hourly timer does about it.

use std::fs;

use crate::DAY;

pub fn header() -> String {
    row("ID", "STATE", "PROJECT", "USED", "EXPIRES")
}

/// One sandbox's line: `left` is seconds until it's deleted.
pub fn line(id: &str, state: &str, project: &str, used: &str, left: i64) -> String {
    row(id, state, project, used, &format!("in {}h", left / 3600))
}

fn row(id: &str, state: &str, project: &str, used: &str, expires: &str) -> String {
    format!("{id:<24} {state:<8} {project:<20} {used:>8}  {expires}")
}

/// Bytes as `numfmt --to=iec` says them: as they are under 1024, else in
/// the largest unit that leaves at least one, rounded up, with a tenth below
/// ten.
pub fn iec(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["K", "M", "G", "T", "P", "E"];
    if bytes < 1024 {
        return bytes.to_string();
    }
    let b = u128::from(bytes);
    let (mut unit, mut base) = (0, 1024u128);
    while b >= base * 1024 {
        base *= 1024;
        unit += 1;
    }
    loop {
        let tenths = (b * 10).div_ceil(base);
        if tenths < 100 {
            return format!("{}.{}{}", tenths / 10, tenths % 10, UNITS[unit]);
        }
        // Rounded up to 1024 of one unit: one of the next.
        let whole = b.div_ceil(base);
        if whole < 1024 || unit + 1 == UNITS.len() {
            return format!("{whole}{}", UNITS[unit]);
        }
        base *= 1024;
        unit += 1;
    }
}

/// Whether a running sandbox gc finds unused is stopped: one that was
/// removed, or that nobody has used for a day.
pub fn stops(removed: bool, idle: i64) -> bool {
    removed || idle > DAY as i64
}

/// What `agent fetch` exiting with `code` says of a sandbox's work: 0 is
/// saved, 2 has nowhere to go (its project isn't a repository of its own),
/// anything else couldn't be saved.
pub fn saved_or_nowhere(code: i32) -> bool {
    matches!(code, 0 | 2)
}

/// Whether gc deletes an expired sandbox, `agent fetch` having exited with
/// `code`: once its work is saved, or once an rm has said it goes. Work in a
/// project that isn't a repository can't be saved, so only an rm lets it go.
pub fn deletes(code: i32, removed: bool) -> bool {
    code == 0 || removed
}

/// The days `agent keep` is given: 7 when none, else a whole number.
pub fn days(given: Option<&str>) -> Option<u32> {
    match given.filter(|d| !d.is_empty()) {
        None => Some(7),
        Some(d) if d.bytes().all(|b| b.is_ascii_digit()) => d.parse().ok(),
        Some(_) => None,
    }
}

/// Whether someone is connected to the sandbox at vsock address `cid` right
/// now: a process (ssh's ProxyCommand) is talking to its sshd.
pub fn in_use(cid: u32) -> bool {
    let wanted = format!("VSOCK-CONNECT:{cid}:22");
    let Ok(procs) = fs::read_dir("/proc") else {
        return false;
    };
    procs.flatten().any(|p| {
        p.file_name()
            .to_string_lossy()
            .bytes()
            .all(|b| b.is_ascii_digit())
            && fs::read(p.path().join("cmdline"))
                .is_ok_and(|c| c.windows(wanted.len()).any(|w| w == wanted.as_bytes()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_is_in_its_columns() {
        assert_eq!(
            header(),
            "ID                       STATE    PROJECT                  USED  EXPIRES"
        );
        assert_eq!(
            line("web-3", "running", "web", "1.5G", 3 * 3600 + 59 * 60),
            "web-3                    running  web                      1.5G  in 3h"
        );
        // Past its expiry, and not yet deleted: hours ago, toward zero.
        assert_eq!(
            line("a-1", "stopped", "a", "?", -3599),
            "a-1                      stopped  a                           ?  in 0h"
        );
        assert!(line("a-1", "stopped", "a", "?", -7200).ends_with("in -2h"));
    }

    #[test]
    fn sizes_are_numfmts() {
        for (bytes, said) in [
            (0, "0"),
            (1023, "1023"),
            (1024, "1.0K"),
            (1025, "1.1K"),
            (1536, "1.5K"),
            (10 * 1024 - 1, "10K"),
            (10 * 1024, "10K"),
            (1024 * 1024 - 1, "1.0M"),
            (123_456_789, "118M"),
            (5 * 1024 * 1024 * 1024, "5.0G"),
            (u64::MAX, "16E"),
            // Past what an f64 holds exactly: one more byte still rounds up.
            ((1 << 60) + 1, "1.1E"),
            ((1 << 58) + 1, "257P"),
        ] {
            assert_eq!(iec(bytes), said, "{bytes}");
        }
    }

    #[test]
    fn gc_stops_what_was_removed_or_is_a_day_unused() {
        assert!(!stops(false, DAY as i64));
        assert!(stops(false, DAY as i64 + 1));
        assert!(stops(true, 0));
        // Used after now (the clock moved back): not idle.
        assert!(!stops(false, -5));
    }

    #[test]
    fn gc_deletes_only_what_was_saved_or_removed() {
        assert!(deletes(0, false));
        assert!(deletes(0, true));
        // A save that failed, or had nowhere to go: kept, unless removed.
        for code in [1, 2, 128 + 15] {
            assert!(!deletes(code, false), "{code}");
            assert!(deletes(code, true), "{code}");
        }
        assert!(saved_or_nowhere(0));
        assert!(saved_or_nowhere(2));
        assert!(!saved_or_nowhere(1));
        assert!(!saved_or_nowhere(128 + 9));
    }

    #[test]
    fn keep_takes_whole_days() {
        assert_eq!(days(None), Some(7));
        assert_eq!(days(Some("")), Some(7));
        assert_eq!(days(Some("0")), Some(0));
        assert_eq!(days(Some("30")), Some(30));
        assert_eq!(days(Some("08")), Some(8));
        for d in ["+3", "-1", "1.5", "two", " 3", "99999999999"] {
            assert_eq!(days(Some(d)), None, "{d}");
        }
    }

    #[test]
    fn a_connection_is_found_by_its_vsock_address() {
        let cid = 4_000_000_000u32 - std::process::id();
        assert!(!in_use(cid));
        let mut held = std::process::Command::new("/bin/sh")
            .args([
                "-c",
                "sleep 30; :",
                &format!("socat - VSOCK-CONNECT:{cid}:22"),
            ])
            .spawn()
            .unwrap();
        let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !in_use(cid) && std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let found = in_use(cid);
        let other = in_use(cid / 10);
        held.kill().unwrap();
        held.wait().unwrap();
        assert!(found);
        assert!(!other, "another address that starts the same isn't it");
    }
}
