//! The owner's notice for each stuck entry: one per entry, raised once, closed
//! once it's gone. It runs after each sweep in a unit of its own, as root, so
//! that HOST-notify (which needs the user's GitHub login) never widens what
//! the sweep may write. It reads only the names in SCRATCH/jobs, and keeps a
//! file per notice it raised in a records dir of its own, so a notice isn't
//! asked for again on every sweep.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::process::{Command, Stdio};

use sha2::{Digest, Sha256};

use crate::STUCK;

/// What every notice key of this starts with.
pub const KEY: &str = "scratch-stuck-";

/// How much of an entry's own name a key and a title show: the rest is the
/// job's user's to choose.
const SHOWN: usize = 64;

/// The most notices of this open at once: a stuck entry past it is noticed
/// once an earlier one closes, so one job's user can't open a flood.
pub const MOST: usize = 20;

/// What `notify` was asked, a line each, for its caller to report.
pub type Asked = Vec<String>;

/// Raises a notice for each stuck entry in `jobs` with none in `records`, and
/// closes each one in `records` whose entry is gone. `notify` is HOST-notify
/// (`ensure`, `close`, `pending`); without one a notice is a journal line.
/// `unit` is the sweep's unit, named in the notice for where to look.
pub fn notice(jobs: &Path, records: &Path, unit: &str, notify: Option<&Path>) -> io::Result<Asked> {
    let stuck: BTreeMap<String, OsString> = fs::read_dir(jobs)?
        .map(|e| e.map(|e| e.file_name()))
        .collect::<io::Result<Vec<_>>>()?
        .into_iter()
        .filter(|n| n.as_bytes().starts_with(STUCK.as_bytes()))
        .map(|n| (key(&n), n))
        .collect();
    let mut asked = Asked::new();
    // Closed first, so what clears makes room under MOST in the same run.
    let mut open = 0;
    for entry in fs::read_dir(records)? {
        let entry = entry?;
        let Some(key) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !key.starts_with(KEY) {
            continue;
        }
        if stuck.contains_key(&key) {
            open += 1;
            continue;
        }
        let why = "It's gone: the sweep removed it.";
        if ask(notify, &["close", &key, why]) && closed(notify, &key) {
            asked.push(format!("close {key}"));
            fs::remove_file(entry.path())?;
        } else {
            open += 1;
        }
    }
    for (key, name) in &stuck {
        let record = records.join(key);
        if record.exists() {
            continue;
        }
        if open >= MOST {
            eprintln!("burrow-scratch: {MOST} notices open already; {key} waits for one to close");
            break;
        }
        let shown = shown(name);
        let title = format!("A finished job's scratch couldn't be removed: {shown}");
        let body = format!(
            "proj set aside a finished job's scratch, and {unit} couldn't remove all of it, so \
             it's now {} and is tried again each time another job's scratch goes. \
             `journalctl -u {unit}` says what stopped it: a mount left inside it, a file \
             marked immutable, or a tree deeper than it goes. Clear that, and the next \
             sweep removes it; this closes by itself then.",
            jobs.join(&shown).display()
        );
        if ask(notify, &["ensure", key, &title, &body]) {
            asked.push(format!("ensure {key}"));
            fs::write(&record, format!("{shown}\n"))?;
            open += 1;
        }
    }
    Ok(asked)
}

/// The notice key for a stuck entry: what it shows of the name, and a hash of
/// all of it, so two names that show the same still have keys of their own.
pub fn key(name: &OsString) -> String {
    let hash = Sha256::digest(name.as_bytes());
    let hex: String = hash[..6].iter().map(|b| format!("{b:02x}")).collect();
    format!("{KEY}{}-{hex}", shown(name))
}

/// An entry's name as a notice shows it: letters, digits, `.`, `-` and `_`
/// only, the rest `_`, and no longer than [`SHOWN`].
fn shown(name: &OsString) -> String {
    name.as_bytes()
        .iter()
        .take(SHOWN)
        .map(|&b| match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'.' | b'-' | b'_' => b as char,
            _ => '_',
        })
        .collect()
}

/// Asks HOST-notify, or with none says it in the journal; whether it was told.
fn ask(notify: Option<&Path>, words: &[&str]) -> bool {
    let Some(notify) = notify else {
        eprintln!("burrow-scratch: {}", words.join(" "));
        return true;
    };
    match Command::new(notify)
        .args(words)
        .stdin(Stdio::null())
        .status()
    {
        Ok(s) if s.success() => true,
        Ok(s) => {
            eprintln!(
                "burrow-scratch: {} {}: {} {s}",
                notify.display(),
                words[0],
                words[1]
            );
            false
        }
        Err(e) => {
            eprintln!("burrow-scratch: {} {}: {e}", notify.display(), words[0]);
            false
        }
    }
}

/// Whether HOST-notify has shown the notice closed (`pending` answers 1), as
/// its other callers check before they forget one.
fn closed(notify: Option<&Path>, key: &str) -> bool {
    let Some(notify) = notify else {
        return true;
    };
    Command::new(notify)
        .args(["pending", key])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .is_ok_and(|s| s.code() == Some(1))
}

#[cfg(test)]
mod tests;
