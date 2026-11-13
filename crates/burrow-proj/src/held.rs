//! A held file lock that ends when its owner lets it go. Closing a file
//! alone doesn't free its lock while a child that another thread forked
//! still has the same open file description, until that child execs: so
//! the lock would outlast its holder, and anyone looking would find it
//! taken by nobody. Every lock proj takes and lets go goes through [`Held`].
//! Slots are the exception: a turn's slot is meant to pass to its child.
use std::{
    fs::{File, TryLockError},
    io,
    os::fd::{AsFd, BorrowedFd},
    thread::sleep,
    time::{Duration, Instant},
};

pub struct Held(File);

impl Held {
    /// Waits for `file`'s lock, exclusive.
    pub fn lock(file: File) -> io::Result<Self> {
        file.lock()?;
        Ok(Self(file))
    }

    /// Waits for `file`'s lock, shared.
    pub fn lock_shared(file: File) -> io::Result<Self> {
        file.lock_shared()?;
        Ok(Self(file))
    }

    /// `file`'s lock, exclusive, if nobody holds it now.
    pub fn try_lock(file: File) -> io::Result<Option<Self>> {
        match file.try_lock() {
            Ok(()) => Ok(Some(Self(file))),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(e)) => Err(e),
        }
    }

    /// `file`'s lock, exclusive, once whoever holds it lets go, if that's
    /// within `wait`.
    pub fn lock_within(file: File, wait: Duration) -> io::Result<Option<Self>> {
        let until = Instant::now() + wait;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Some(Self(file))),
                Err(TryLockError::WouldBlock) if Instant::now() < until => {
                    sleep(Duration::from_millis(100));
                }
                Err(TryLockError::WouldBlock) => return Ok(None),
                Err(TryLockError::Error(e)) => return Err(e),
            }
        }
    }
}

impl AsFd for Held {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letting_go_frees_the_lock_a_forked_child_still_shares() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lock");
        let open = || {
            File::options()
                .create(true)
                .append(true)
                .open(&path)
                .unwrap()
        };
        for mode in ["exclusive", "shared", "try"] {
            let held = match mode {
                "exclusive" => Held::lock(open()).unwrap(),
                "shared" => Held::lock_shared(open()).unwrap(),
                "try" => Held::try_lock(open()).unwrap().unwrap(),
                _ => unreachable!(),
            };
            // A dup shares the open file description just as a descriptor in
            // a concurrently forked child does, until that child reaches exec.
            let inherited = held.as_fd().try_clone_to_owned().unwrap();
            assert!(Held::try_lock(open()).unwrap().is_none(), "{mode}");
            drop(held);
            assert!(Held::try_lock(open()).unwrap().is_some(), "{mode}");
            drop(inherited);
        }
    }
}
