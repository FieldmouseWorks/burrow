//! Where the inbox keeps an issue's state: the folders ([`worker`],
//! [`sorting`] and the rest) and the files in a record folder ([`File`]).
//! Their names are here alone. A reader asks the record what a file says
//! (one reading each, below); a step that changes the record writes it
//! through [`Mark`], which can't read.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::{Fail, mtime, read};

/// The issue's record folder (KEY-N in the inbox's state).
pub(in crate::inbox) fn worker(state: &Path, key: &str, n: &str) -> PathBuf {
    state.join(format!("{key}-{n}"))
}
/// Its sorting record.
pub(in crate::inbox) fn sorting(state: &Path, key: &str, n: &str) -> PathBuf {
    sorting_root(state).join(format!("{key}-{n}"))
}
/// The sorting records.
pub(in crate::inbox) fn sorting_root(state: &Path) -> PathBuf {
    state.join(".triage")
}
/// Why it waits to be sorted, while it does.
pub(in crate::inbox) fn sorting_waits(state: &Path) -> PathBuf {
    state.join(".triage-waiting")
}
/// Each repo's ready issues, as its last pass listed them.
pub(in crate::inbox) fn ready_lists(state: &Path) -> PathBuf {
    state.join(".ready")
}
/// Since when GitHub has refused each repo.
pub(in crate::inbox) fn unreached(state: &Path) -> PathBuf {
    state.join(".unreached")
}
/// The issues whose labels parked their job.
pub(in crate::inbox) fn parked(state: &Path, key: &str, n: &str) -> PathBuf {
    parked_root(state).join(key.to_lowercase()).join(n)
}
/// Every repo's parked issues.
pub(in crate::inbox) fn parked_root(state: &Path) -> PathBuf {
    state.join(".parked")
}
/// The records of the issue's retired jobs.
pub(in crate::inbox) fn retired(state: &Path, key: &str, n: &str) -> PathBuf {
    retired_root(state).join(key.to_lowercase()).join(n)
}
/// Every repo's retired records.
pub(in crate::inbox) fn retired_root(state: &Path) -> PathBuf {
    state.join(".retired")
}
/// The drain's mark while a ready issue waits for a slot.
pub(in crate::inbox) fn drain_waiting(state: &Path) -> PathBuf {
    state.join(".drain-waiting")
}
/// Issues skipped because an open closing PR already carries them.
pub(in crate::inbox) fn carried_root(state: &Path) -> PathBuf {
    state.join(".carried")
}
/// One carried issue's record file (`OWNER/REPO#N\tPR-URL`).
pub(in crate::inbox) fn carried_path(state: &Path, key: &str, n: &str) -> PathBuf {
    carried_root(state).join(format!("{}-{n}", key.to_lowercase()))
}
/// The open closing PR carrying `key#n`, if recorded.
pub(in crate::inbox) fn carried(state: &Path, key: &str, n: &str) -> Option<String> {
    carried_entry(&carried_path(state, key, n)).map(|(_, pr)| pr)
}
/// `(issue, pr_url)` from a `.carried/` record file.
pub(in crate::inbox) fn carried_entry(path: &Path) -> Option<(String, String)> {
    let text = read(path)?;
    let (issue, pr) = text.split_once('\t')?;
    let pr = pr.trim();
    (!issue.is_empty() && !pr.is_empty()).then(|| (issue.to_owned(), pr.to_owned()))
}
/// When `key#n` was recorded as carried by an open closing PR.
pub(in crate::inbox) fn carried_since(state: &Path, key: &str, n: &str) -> Option<i64> {
    mtime(&carried_path(state, key, n))
}
/// A file in an issue's record folder: its worker record, its sorting
/// record, or a retired one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum File {
    /// The job it was handed to (empty while a sorting record waits for one).
    Job,
    /// The issue, OWNER/REPO#N.
    Issue,
    /// The latest turn whose answer was posted on the issue.
    Answered,
    /// The newest comment handed on.
    Seen,
    /// That the drain handed it on: the issue, OWNER/REPO#N.
    Drained,
    /// What its job was last handed of the issue.
    Input,
    /// That the inbox refused it, and said so on the issue.
    Refused,
    /// A sorting verdict's steps, as first planned.
    Plan,
    /// How many of those steps GitHub took.
    Did,
    /// That its sorting job gave no verdict: the issue's `updated` then.
    Failed,
    /// When the issue was last updated, as its sorting job was handed it.
    Updated,
    /// The question its sorting job asked, as noticed on the issue.
    QuestionNotice,
    /// The turn the last carrying on of its job started, and the streak.
    Revived,
    /// That its job hasn't been said on the issue yet.
    Unannounced,
    /// The stopped job the issue was told about.
    StoppedTold,
    /// That a retired record's last answer was posted.
    Reported,
}

impl File {
    fn name(self) -> &'static str {
        match self {
            File::Job => "job",
            File::Issue => "issue",
            File::Answered => "answered",
            File::Seen => "seen",
            File::Drained => "drained",
            File::Input => "input",
            File::Refused => "refused",
            File::Plan => "plan",
            File::Did => "did",
            File::Failed => "failed",
            File::Updated => "updated",
            File::QuestionNotice => "question-notice",
            File::Revived => "revived",
            File::Unannounced => "unannounced",
            File::StoppedTold => "stopped-told",
            File::Reported => "reported",
        }
    }
}

/// Tests build a record folder by hand: `d.join(File::Job)`. Only theirs,
/// so no reader can take a path from it.
#[cfg(test)]
impl AsRef<Path> for File {
    fn as_ref(&self) -> &Path {
        Path::new(self.name())
    }
}

fn path(d: &Path, f: File) -> PathBuf {
    d.join(f.name())
}

/// A record file, as a step that changes the record writes it: it puts,
/// swaps or removes the file and never reads it. Its path shows only in
/// what a failed write says.
pub(in crate::inbox) struct Mark(PathBuf);

/// Record folder `d`'s file `f`, to write.
pub(in crate::inbox) fn mark(d: &Path, f: File) -> Mark {
    Mark(path(d, f))
}

impl Mark {
    /// Written in place (`fs::write`).
    pub(in crate::inbox) fn write(&self, text: impl AsRef<[u8]>) -> io::Result<()> {
        fs::write(&self.0, text)
    }

    /// Written in place, or a failure naming the file.
    pub(in crate::inbox) fn put(&self, text: impl AsRef<[u8]>) -> Result<(), Fail> {
        self.write(text).map_err(|e| self.fail(e))
    }

    /// Swapped in through its `.new` ([`crate::swap`]).
    pub(in crate::inbox) fn swap(&self, text: &str) -> io::Result<()> {
        crate::swap(&self.0, text)
    }

    /// Removed (`fs::remove_file`).
    pub(in crate::inbox) fn remove(&self) -> io::Result<()> {
        fs::remove_file(&self.0)
    }

    /// A failed write, naming the file, as [`crate::io_fail`] says it.
    pub(in crate::inbox) fn fail(&self, e: io::Error) -> Fail {
        crate::io_fail(&self.0, e)
    }
}

impl fmt::Display for Mark {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.0.display().fmt(f)
    }
}

// What record folder `d` says, one reading each: text as written (its
// trailing newlines off), whether a mark is there, and since when.

/// Its job, as written.
pub(in crate::inbox) fn job(d: &Path) -> Option<String> {
    read(&path(d, File::Job))
}
/// Whether its job is written, empty or not.
pub(in crate::inbox) fn has_job(d: &Path) -> bool {
    path(d, File::Job).exists()
}
/// When its job was written.
pub(in crate::inbox) fn job_since(d: &Path) -> Option<i64> {
    mtime(&path(d, File::Job))
}
/// Its issue, OWNER/REPO#N, as written.
pub(in crate::inbox) fn issue(d: &Path) -> Option<String> {
    read(&path(d, File::Issue))
}
/// The latest turn answered on the issue, as written.
pub(in crate::inbox) fn answered(d: &Path) -> Option<String> {
    read(&path(d, File::Answered))
}
/// When a turn was last answered on the issue.
pub(in crate::inbox) fn answered_since(d: &Path) -> Option<i64> {
    mtime(&path(d, File::Answered))
}
/// The newest comment handed on, as written.
pub(in crate::inbox) fn seen(d: &Path) -> Option<String> {
    read(&path(d, File::Seen))
}
/// Whether the drain handed it on.
pub(in crate::inbox) fn drained(d: &Path) -> bool {
    path(d, File::Drained).exists()
}
/// The issue the drain handed on, as written.
pub(in crate::inbox) fn drained_for(d: &Path) -> Option<String> {
    read(&path(d, File::Drained))
}
/// When the drain handed it on.
pub(in crate::inbox) fn drained_since(d: &Path) -> Option<i64> {
    mtime(&path(d, File::Drained))
}
/// What its job was last handed of the issue, exactly.
pub(in crate::inbox) fn input(d: &Path) -> io::Result<String> {
    fs::read_to_string(path(d, File::Input))
}
/// Whether what its job was handed is kept.
pub(in crate::inbox) fn has_input(d: &Path) -> bool {
    path(d, File::Input).exists()
}
/// Whether the inbox refused it.
pub(in crate::inbox) fn refused(d: &Path) -> bool {
    path(d, File::Refused).exists()
}
/// When the inbox refused it.
pub(in crate::inbox) fn refused_since(d: &Path) -> Option<i64> {
    mtime(&path(d, File::Refused))
}
/// Its sorting verdict's steps, as kept.
pub(in crate::inbox) fn plan(d: &Path) -> Option<Vec<u8>> {
    fs::read(path(d, File::Plan)).ok()
}
/// Whether its sorting verdict's steps are kept.
pub(in crate::inbox) fn has_plan(d: &Path) -> bool {
    path(d, File::Plan).is_file()
}
/// How many of its steps GitHub took, as written.
pub(in crate::inbox) fn did(d: &Path) -> Option<String> {
    read(&path(d, File::Did))
}
/// Whether its sorting job gave no verdict.
pub(in crate::inbox) fn failed(d: &Path) -> bool {
    path(d, File::Failed).exists()
}
/// The issue's `updated` its sorting job failed on, as written.
pub(in crate::inbox) fn failed_on(d: &Path) -> Option<String> {
    read(&path(d, File::Failed))
}
/// When its sorting job failed.
pub(in crate::inbox) fn failed_since(d: &Path) -> Option<i64> {
    mtime(&path(d, File::Failed))
}
/// When the issue was last updated as its sorting job was handed it.
pub(in crate::inbox) fn updated(d: &Path) -> Option<String> {
    read(&path(d, File::Updated))
}
/// The question noticed on the issue, as written.
pub(in crate::inbox) fn question_notice(d: &Path) -> Option<String> {
    read(&path(d, File::QuestionNotice))
}
/// Its job's last carrying on, "TURN STREAK", as written.
pub(in crate::inbox) fn revived(d: &Path) -> Option<String> {
    read(&path(d, File::Revived))
}
/// Whether its job is still to be said on the issue.
pub(in crate::inbox) fn unannounced(d: &Path) -> bool {
    path(d, File::Unannounced).exists()
}
/// The stopped job the issue was told about, as written.
pub(in crate::inbox) fn stopped_told(d: &Path) -> Option<String> {
    read(&path(d, File::StoppedTold))
}
/// Whether a retired record's last answer was posted.
pub(in crate::inbox) fn reported(d: &Path) -> bool {
    path(d, File::Reported).exists()
}

/// The issue of the record folder in the inbox's `state` that names job
/// `id`, OWNER/REPO#N as written: for whoever outside the inbox asks.
pub(crate) fn job_issue(state: &Path, id: &str) -> Option<String> {
    for entry in fs::read_dir(state).ok()?.flatten() {
        let d = entry.path();
        if job(&d).as_deref() == Some(id) {
            return issue(&d);
        }
    }
    None
}

/// Each record folder in the inbox's `state` that names job `id` let go by
/// the drain: its `drained` mark removed.
pub(crate) fn undrain(state: &Path, id: &str) {
    for entry in fs::read_dir(state).into_iter().flatten().flatten() {
        let d = entry.path();
        if job(&d).as_deref() == Some(id) {
            let _ = mark(&d, File::Drained).remove();
        }
    }
}
