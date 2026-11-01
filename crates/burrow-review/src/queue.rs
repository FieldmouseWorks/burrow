//! The queue the steward's passes fill and this service empties, the
//! answers it leaves, and the names that tie them to a PR's session. An
//! entry is an `Item`, which burrow-upkeep's `queue` writes, at
//! `REVIEWER--SNAP.json`, where SNAP is `pr-OWNER_REPO-N-HEAD12-BASEHASH`
//! (reviewers.py `snap_name`); the answer goes in done/ under the same
//! name.

use std::fs::File;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The prefix the steward's sweep (burrow-upkeep's sweep.rs) gives a
/// session once its PR has closed or been retargeted: this service then
/// deletes it (work.rs `forget`).
pub const GONE: &str = ".gone-";

/// A queued look.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub reviewer: String,
    /// The snapshot it reads, by name.
    pub snap: String,
    pub repo: String,
    pub number: u64,
    pub head_ref_oid: String,
    pub base_ref_name: String,
    /// The reviewer's brief, whole.
    pub prompt: String,
    /// What the answer carries of the description the brief quotes.
    pub description: String,
    /// When the PR was opened, and when the look was queued: the order
    /// the queue is taken in (work.rs `work`).
    pub opened: f64,
    pub at: f64,
}

/// The reviewer's session for the PR, on its base, whose head the snapshot
/// `snap` is: the same for every head, so each look resumes the last one's,
/// and another for a retargeted PR, which is another change. None for a
/// name that isn't a snapshot's.
pub fn session_name(reviewer: &str, snap: &str) -> Option<String> {
    let mut parts = snap.rsplitn(3, '-');
    let base = parts.next()?;
    parts.next()?;
    let pr = parts.next()?;
    Some(format!("{reviewer}--{pr}-{base}"))
}

/// The reviewer a queue entry is for: the name up to its first `--`.
pub fn reviewer_of(name: &str) -> &str {
    name.split_once("--").map_or(name, |(r, _)| r)
}

/// The session a queued look is in: one whose name isn't a snapshot's is
/// its own, and fails as no review.
pub fn session_of(name: &str) -> String {
    let stem = name.strip_suffix(".json").unwrap_or(name);
    let (r, snap) = stem.split_once("--").unwrap_or((stem, ""));
    session_name(r, snap).unwrap_or_else(|| name.to_owned())
}

/// p gone, whether this takes it or something else got there first.
pub fn remove(p: &Path) -> io::Result<()> {
    let gone = match std::fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(p),
        Ok(_) => std::fs::remove_file(p),
        Err(e) => Err(e),
    };
    match gone {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// A queue entry, open to lock: a file, never a directory in its place.
pub fn open(path: &Path) -> io::Result<File> {
    let f = File::open(path)?;
    if f.metadata()?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::IsADirectory,
            "it's a directory",
        ));
    }
    Ok(f)
}

/// Takes the queued look at path off the queue unless a worker is on it (it
/// holds the entry's lock while it is), as reviewers.py `taken` does. False
/// if one is, or if it can't be opened to tell.
pub fn taken(path: &Path) -> bool {
    let f = match open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return true,
        Err(_) => return false,
    };
    if f.try_lock().is_err() {
        return false;
    }
    remove(path).is_ok()
}

/// What a JSON file holds, if it's there and reads as JSON.
pub fn load(path: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// value, written whole: a reader sees the old file or the new one.
pub fn save(path: &Path, value: &Value) -> io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(value)?)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::{reviewer_of, session_name, session_of};

    /// Every head of a PR on one base shares a session; another base, or
    /// another PR, has its own; a name that isn't a snapshot's is its own.
    #[test]
    fn a_prs_heads_share_its_session() {
        let one = "pr-sam_r-5-aaaaaaaaaaaa-1234abcd";
        let two = "pr-sam_r-5-bbbbbbbbbbbb-1234abcd";
        assert_eq!(
            session_name("codex", one).as_deref(),
            Some("codex--pr-sam_r-5-1234abcd")
        );
        assert_eq!(session_name("codex", one), session_name("codex", two));
        assert_ne!(
            session_name("codex", one),
            session_name("codex", "pr-sam_r-5-aaaaaaaaaaaa-99999999")
        );
        assert_ne!(
            session_name("codex", one),
            session_name("codex", "pr-sam_r-6-aaaaaaaaaaaa-1234abcd")
        );
        assert_eq!(
            session_of(&format!("codex--{one}.json")),
            "codex--pr-sam_r-5-1234abcd"
        );
        assert_eq!(session_of("codex--unread.json"), "codex--unread.json");
        assert_eq!(session_name("codex", "a-b"), None);
        assert_eq!(reviewer_of("codex--x.json"), "codex");
        assert_eq!(reviewer_of("stray.json"), "stray.json");
    }
}
