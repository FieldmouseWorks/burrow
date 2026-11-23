//! A run waiting in line, for proj's turn: when the command runs in a proj
//! turn (PROJ_WAIT_DIR is set), its file there says what it waits for, and
//! holds an exclusive flock(2) while it waits, so proj stops the turn's clock
//! for the wait (burrow-proj's waits.rs). The file goes once the run has its
//! slot or gives up, and so does a run that dies, its lock with it.

use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::PathBuf;

use rustix::fs::{FlockOperation, flock};

/// The file of a run that waits, removed when it's dropped.
pub(crate) struct Waiting(Option<(PathBuf, File)>);

impl Waiting {
    /// Says the run waits for `what`, if its turn has a wait dir. A file
    /// that can't be made is no reason to refuse the run, so it's skipped.
    pub(crate) fn start(what: &str) -> Waiting {
        let Some(dir) = std::env::var_os("PROJ_WAIT_DIR") else {
            return Waiting(None);
        };
        let path = PathBuf::from(dir).join(format!("heavy.{}", std::process::id()));
        let Ok(mut file) = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
        else {
            return Waiting(None);
        };
        if writeln!(file, "{what}").is_err()
            || flock(&file, FlockOperation::NonBlockingLockExclusive).is_err()
        {
            let _ = fs::remove_file(&path);
            return Waiting(None);
        }
        Waiting(Some((path, file)))
    }
}

impl Drop for Waiting {
    fn drop(&mut self) {
        if let Some((path, _file)) = self.0.take() {
            let _ = fs::remove_file(path);
        }
    }
}
