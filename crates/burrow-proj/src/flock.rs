//! A file lock that ends when its holder lets go. Closing alone doesn't end
//! a flock: a child any thread of the process forked while it was open shares
//! the same open file description until that child execs, and holds the lock
//! till then. Every lock proj holds for a span is one of these.
use std::fs::File;
use std::os::fd::{AsFd, BorrowedFd};

pub struct Flock(File);

impl Flock {
    /// `file`, already locked (exclusive or shared) by the caller.
    pub(crate) fn held(file: File) -> Self {
        Self(file)
    }
}

impl AsFd for Flock {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl Drop for Flock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::TryLockError;

    #[test]
    fn letting_go_frees_the_lock_while_a_forked_copy_is_open() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("lock");
        let file = File::create(&path).unwrap();
        file.lock().unwrap();
        // What a child forked now holds until it execs, kept open on purpose.
        let inherited = file.try_clone().unwrap();
        let held = Flock::held(file);
        let next = File::options().append(true).open(&path).unwrap();
        assert!(matches!(next.try_lock(), Err(TryLockError::WouldBlock)));
        drop(held);
        next.try_lock()
            .expect("let go, with the forked copy still open");
        drop(inherited);
    }
}
