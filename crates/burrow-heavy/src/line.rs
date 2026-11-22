//! The line for the slots, so a slot freed goes to whoever has waited
//! longest: a run takes its place before it first looks for a slot, and
//! looks only while nobody is ahead of it. `--all` goes ahead of `--first`,
//! which goes ahead of everyone else, and each is in order among its own.
//!
//! A place is a read lock on one byte of the slots' directory (fcntl(2)'s
//! open file description locks, apart from flock's): further along for a
//! later class, and within one by when it came on the boot clock, which
//! setting the time doesn't move. Like a slot it needs no more than reading,
//! so runners with a read-only /run stand in the same line, and the kernel
//! gives it back however its holder ends. One that's stopped keeps it, as
//! it would keep a slot. (A blocking flock keeps no such order: a newcomer
//! takes the lock while the waiter it was handed to is still waking.)

use std::fs::File;
use std::path::Path;

use nix::fcntl::{FcntlArg, fcntl};
use nix::libc;
use rustix::time::{ClockId, clock_gettime};

use crate::{note, open};

/// Who a run is in the line, those served first first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// `--all`: the machine to itself.
    All,
    /// `--first`: what the machine's own deploy waits on.
    First,
    /// Everyone else.
    Ordinary,
}

/// The bytes a class's places take: nanoseconds for 73 years of one boot.
const CLASS_BITS: u32 = 61;

/// A place in the line, held until it's dropped.
pub(crate) struct Place {
    file: File,
    at: i64,
}

impl Place {
    /// A place at the end of `class`'s part of the line in the slots'
    /// `dir`; an error is the exit status.
    pub(crate) fn take(dir: &Path, class: Class) -> Result<Place, u8> {
        let file = open(dir)?;
        let now = clock_gettime(ClockId::Boottime);
        let at = ((class as i64) << CLASS_BITS) + now.tv_sec * 1_000_000_000 + now.tv_nsec;
        let mine = bytes(libc::F_RDLCK, at, 1);
        fcntl(&file, FcntlArg::F_OFD_SETLK(&mine)).map_err(|e| {
            note!("heavy: cannot get in line in {}: {e}", dir.display());
            1
        })?;
        Ok(Place { file, at })
    }

    /// Whether nobody is ahead: no other lock on any byte before this one's.
    pub(crate) fn is_next(&self) -> Result<bool, u8> {
        let mut ahead = bytes(libc::F_WRLCK, 0, self.at);
        fcntl(&self.file, FcntlArg::F_OFD_GETLK(&mut ahead)).map_err(|e| {
            note!("heavy: cannot read the line: {e}");
            1
        })?;
        Ok(i32::from(ahead.l_type) == libc::F_UNLCK)
    }
}

/// `len` bytes from `start`, as fcntl takes a lock.
fn bytes(kind: libc::c_int, start: i64, len: i64) -> libc::flock {
    libc::flock {
        l_type: kind as libc::c_short,
        l_whence: libc::SEEK_SET as libc::c_short,
        l_start: start,
        l_len: len,
        l_pid: 0,
    }
}
