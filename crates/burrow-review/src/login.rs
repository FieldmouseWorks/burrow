//! Codex's login, and every key it has held. A refresh rotates the login's
//! keys and a look may then quote them, so the service has to know each one
//! before anything it says goes anywhere; a file several looks write can
//! lose one to the next write before anyone reads it, whatever watches it.
//!
//! So no look shares a file. Each is lent a copy of its own, a regular file
//! in its PR's CODEX_HOME that only it writes, in place or by saving a new
//! file over it, and the service stays the only one that moves keys between
//! looks: it reads a look's copy, keeps its keys, puts it in the login, and
//! only then hands it to the looks beside it, which need it because a
//! refresh spends the key before it. A look can only say what it was lent,
//! which is already kept, or what it wrote itself, which is read on each
//! write (`Watch`) and once more when it ends, before anything it said is
//! read.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{self, Read, Seek, Write};
use std::mem::MaybeUninit;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use rustix::fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags};
use rustix::fs::{AtFlags, CWD, Mode, OFlags, RenameFlags};
use serde_json::Value;

use crate::{Log, held};

/// Nothing shorter is a key, an empty value least of all.
const SHORTEST: usize = 16;

/// The login's name, in its directory and in each look's CODEX_HOME.
pub const LOGIN: &str = "auth.json";

/// Where a login is written before it's swapped in, and where what it
/// displaced is read from.
const TMP: &str = "auth.json.burrow";

/// A login as bytes, if they're a whole one: a write still under way isn't.
fn whole(bytes: Vec<u8>) -> Option<(Vec<u8>, Value)> {
    let v = serde_json::from_slice(&bytes).ok()?;
    Some((bytes, v))
}

/// Every string in a login long enough to be a key.
fn keys_in(x: &Value, found: &mut BTreeSet<String>) {
    match x {
        Value::String(s) if s.chars().count() >= SHORTEST => {
            found.insert(s.clone());
        }
        Value::Object(m) => m.values().for_each(|v| keys_in(v, found)),
        Value::Array(a) => a.iter().for_each(|v| keys_in(v, found)),
        _ => {}
    }
}

/// `name` in dir, if it's a regular file. Never a pipe or a link: what's in
/// a look's home is the look's to have left there.
fn open_file(dir: &OwnedFd, name: &str) -> io::Result<Option<File>> {
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    match rustix::fs::openat(dir, name, flags, Mode::empty()) {
        Ok(fd) => {
            let f = File::from(fd);
            Ok(f.metadata()?.is_file().then_some(f))
        }
        Err(rustix::io::Errno::NOENT | rustix::io::Errno::LOOP | rustix::io::Errno::NXIO) => {
            Ok(None)
        }
        Err(e) => Err(e.into()),
    }
}

/// All of an open file, from its start.
fn all_of(mut f: &File) -> io::Result<Vec<u8>> {
    f.rewind()?;
    let mut b = Vec::new();
    f.read_to_end(&mut b)?;
    Ok(b)
}

/// bytes as `LOGIN` in dir, whole at once, for its owner alone; and the file
/// they took the place of, whatever was there at that very moment. Swapped,
/// not moved over: a save just before this is in what comes back, not lost.
fn swap(dir: &OwnedFd, bytes: &[u8]) -> io::Result<Option<File>> {
    gone(dir, TMP)?;
    let fd = rustix::fs::openat(
        dir,
        TMP,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )?;
    File::from(fd).write_all(bytes)?;
    loop {
        match rustix::fs::renameat_with(dir, TMP, dir, LOGIN, RenameFlags::EXCHANGE) {
            Ok(()) => {
                let old = open_file(dir, TMP)?;
                gone(dir, TMP)?;
                return Ok(old);
            }
            Err(rustix::io::Errno::NOENT) => {
                match rustix::fs::renameat_with(dir, TMP, dir, LOGIN, RenameFlags::NOREPLACE) {
                    Ok(()) => return Ok(None),
                    // Saved in the moment since: that one's swapped.
                    Err(rustix::io::Errno::EXIST) => {}
                    Err(e) => return Err(e.into()),
                }
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// `name` in dir removed, if it's there.
fn gone(dir: &OwnedFd, name: &str) -> io::Result<()> {
    match rustix::fs::unlinkat(dir, name, AtFlags::empty()) {
        Ok(()) | Err(rustix::io::Errno::NOENT) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

fn open_dir(dir: &Path) -> io::Result<OwnedFd> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    Ok(rustix::fs::openat(CWD, dir, flags, Mode::empty())?)
}

/// A look's copy of the login.
struct Copy {
    /// Its CODEX_HOME, open: a session renamed under it is still the one read.
    home: OwnedFd,
    /// What it last held.
    last: Option<Vec<u8>>,
    /// The copies the service has put new ones over, each with what it last
    /// held: a write the look began before that lands in one of these.
    replaced: Vec<(File, Vec<u8>)>,
}

#[derive(Default)]
struct State {
    /// The login's directory.
    dir: PathBuf,
    /// The login as the service last read or wrote it there.
    shared: Option<Vec<u8>>,
    /// The latest login anyone has: what the login and each copy should hold.
    current: Option<Vec<u8>>,
    looks: BTreeMap<u64, Copy>,
    next: u64,
    seen: BTreeSet<String>,
}

impl State {
    /// bytes, if they're a login that's new here: its keys kept, and it the
    /// latest.
    fn take_in(&mut self, bytes: Vec<u8>, known: Option<&Vec<u8>>) -> Option<Vec<u8>> {
        if known == Some(&bytes) {
            return None;
        }
        let (bytes, v) = whole(bytes)?;
        keys_in(&v, &mut self.seen);
        self.current = Some(bytes.clone());
        Some(bytes)
    }

    /// Everyone's login read, each new one's keys kept, then the latest
    /// handed to whoever hasn't got it. A look's write wins over the login
    /// file's: the look can only have made it from what it was lent.
    fn sync(&mut self) -> io::Result<()> {
        self.read_all()?;
        self.hand_over()
    }

    fn read_all(&mut self) -> io::Result<()> {
        match std::fs::read(self.dir.join(LOGIN)) {
            Ok(bytes) => {
                let known = self.shared.clone();
                if let Some(b) = self.take_in(bytes, known.as_ref()) {
                    self.shared = Some(b);
                }
            }
            // Logged out on the machine: the looks at it keep what they have.
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                self.shared = None;
                self.current = None;
            }
            Err(e) => return Err(e),
        }
        let mut looks = std::mem::take(&mut self.looks);
        let read = looks.values_mut().try_for_each(|l| self.read_copy(l));
        self.looks = looks;
        read
    }

    /// The latest login to whoever hasn't got it. What each hand-over
    /// displaces is read as it comes back: a save made since they were all
    /// read is the latest then, and goes round in its turn.
    fn hand_over(&mut self) -> io::Result<()> {
        loop {
            let Some(current) = self.current.clone() else {
                return Ok(());
            };
            let mut news = false;
            if self.shared.as_ref() != Some(&current) {
                let old = swap(&open_dir(&self.dir)?, &current)?;
                let was = self.shared.replace(current.clone());
                if let Some(f) = old {
                    news |= self.take_in(all_of(&f)?, was.as_ref()).is_some();
                }
            }
            let mut looks = std::mem::take(&mut self.looks);
            let handed = looks.values_mut().try_for_each(|l| {
                news |= self.hand_to(l, &current)?;
                Ok::<_, io::Error>(())
            });
            self.looks = looks;
            handed?;
            if !news {
                return Ok(());
            }
        }
    }

    /// current in a look's copy, if it isn't there. The file it displaces is
    /// kept open, for a write the look began before that; whether that file
    /// holds a login not seen yet is what comes back.
    fn hand_to(&mut self, l: &mut Copy, current: &[u8]) -> io::Result<bool> {
        if l.last.as_deref() == Some(current) {
            return Ok(false);
        }
        let old = swap(&l.home, current)?;
        let was = l.last.replace(current.to_owned());
        let Some(f) = old else {
            return Ok(false);
        };
        let new = self.take_in(all_of(&f)?, was.as_ref());
        let news = new.is_some();
        l.replaced.push((f, new.or(was).unwrap_or_default()));
        Ok(news)
    }

    /// What a look has written since it was last read.
    fn read_copy(&mut self, l: &mut Copy) -> io::Result<()> {
        for (f, was) in &mut l.replaced {
            if let Some(b) = self.take_in(all_of(f)?, Some(was)) {
                *was = b;
            }
        }
        if let Some(f) = open_file(&l.home, LOGIN)?
            && let Some(b) = self.take_in(all_of(&f)?, l.last.as_ref())
        {
            l.last = Some(b);
        }
        Ok(())
    }

    /// A copy for the look at this CODEX_HOME, over whatever's there: a link
    /// from before, or a copy a look killed with the service left, whose
    /// keys are still kept.
    fn lend(&mut self, home: OwnedFd) -> io::Result<u64> {
        self.sync()?;
        if let Some(f) = open_file(&home, LOGIN)?
            && let Some((_, v)) = whole(all_of(&f)?)
        {
            keys_in(&v, &mut self.seen);
        }
        match &self.current {
            Some(c) => drop(swap(&home, c)?),
            None => gone(&home, LOGIN)?,
        }
        let id = self.next;
        self.next += 1;
        let last = self.current.clone();
        self.looks.insert(
            id,
            Copy {
                home,
                last,
                replaced: Vec::new(),
            },
        );
        Ok(id)
    }

    /// The look done: what it last wrote kept and in the login, its copy gone.
    fn give_back(&mut self, id: u64) -> io::Result<()> {
        let synced = self.sync();
        if let Some(l) = self.looks.remove(&id)
            && synced.is_ok()
        {
            gone(&l.home, LOGIN)?;
        }
        synced
    }
}

/// Codex's login (`HOME/.codex/auth.json`), the looks' copies of it, and
/// every key they've held since the service began.
pub struct Logins {
    state: Arc<Mutex<State>>,
    log: Log,
}

impl Logins {
    pub fn new(home: &Path, log: Log) -> Logins {
        let state = State {
            dir: home.join(".codex"),
            ..State::default()
        };
        Logins {
            state: Arc::new(Mutex::new(state)),
            log,
        }
    }

    /// A thread that syncs on each write to `LOGIN` in dir.
    fn watching(&self, dir: &Path) -> io::Result<Watch> {
        let (state, log) = (self.state.clone(), self.log.clone());
        Watch::start(dir, move || {
            if let Err(e) = held(&state).sync() {
                log(&format!("couldn't read the login: {e}"));
            }
        })
    }

    /// The login's directory watched until what's returned is dropped, for
    /// a login redone on the machine; nothing to watch if it isn't there.
    pub fn watch(&self) -> Option<Watch> {
        let dir = held(&self.state).dir.clone();
        match self.watching(&dir) {
            Ok(w) => Some(w),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => {
                (self.log)(&format!("couldn't watch the login: {e}"));
                None
            }
        }
    }

    /// A copy of the login in `home`, a look's CODEX_HOME, watched from
    /// before the look can write it until it's given back.
    pub fn lend(&self, home: &Path) -> io::Result<Lent<'_>> {
        let fd = open_dir(home)?;
        let watch = self.watching(home)?;
        let id = held(&self.state).lend(fd)?;
        Ok(Lent {
            logins: self,
            id,
            watch: Some(watch),
            back: false,
        })
    }

    /// Every key seen, with the login's and each look's as they are now,
    /// the longest first, so none is left half out by a shorter one in it.
    pub fn keys(&self) -> Vec<String> {
        let mut state = held(&self.state);
        if let Err(e) = state.sync() {
            (self.log)(&format!("couldn't read the login: {e}"));
        }
        let mut keys: Vec<String> = state.seen.iter().cloned().collect();
        keys.sort_by_key(|k| std::cmp::Reverse(k.len()));
        keys
    }
}

/// A look's copy of the login, until the look is done with it.
pub struct Lent<'a> {
    logins: &'a Logins,
    id: u64,
    watch: Option<Watch>,
    back: bool,
}

impl Lent<'_> {
    fn end(&mut self) -> io::Result<()> {
        self.back = true;
        drop(self.watch.take());
        held(&self.logins.state).give_back(self.id)
    }

    /// Done: an error says a refresh couldn't be put back in the login.
    pub fn give_back(mut self) -> io::Result<()> {
        self.end()
    }
}

impl Drop for Lent<'_> {
    fn drop(&mut self) {
        if !self.back
            && let Err(e) = self.end()
        {
            (self.logins.log)(&format!("couldn't keep its login: {e}"));
        }
    }
}

/// `f` called on each write to `LOGIN` in a directory, for as long as this
/// is held, as soon after the write as it can be: in place, or a new file
/// saved over it.
pub struct Watch {
    inotify: Arc<OwnedFd>,
    dir: i32,
    thread: Option<JoinHandle<()>>,
}

impl Watch {
    fn start(dir: &Path, f: impl Fn() + Send + 'static) -> io::Result<Watch> {
        let inotify = Arc::new(inotify::init(CreateFlags::CLOEXEC)?);
        let wd = inotify::add_watch(
            &*inotify,
            dir,
            WatchFlags::MODIFY | WatchFlags::CLOSE_WRITE | WatchFlags::MOVED_TO,
        )?;
        let fd = inotify.clone();
        let thread = thread::spawn(move || {
            let mut buf = [MaybeUninit::uninit(); 4096];
            let mut events = inotify::Reader::new(&*fd, &mut buf);
            loop {
                match events.next() {
                    // The watch removed: by drop, or with its directory.
                    Ok(e) if e.events().contains(ReadFlags::IGNORED) => return,
                    Ok(e)
                        if e.file_name()
                            .is_some_and(|n| n.to_bytes() == LOGIN.as_bytes()) =>
                    {
                        f();
                    }
                    Ok(_) | Err(rustix::io::Errno::INTR) => {}
                    Err(_) => return,
                }
            }
        });
        Ok(Watch {
            inotify,
            dir: wd,
            thread: Some(thread),
        })
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        // The watch removed is the thread's last event; one that's gone
        // already took the thread with it.
        let removed = inotify::remove_watch(&*self.inotify, self.dir).is_ok();
        if let Some(t) = self.thread.take()
            && (removed || t.is_finished())
        {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
#[path = "login_tests.rs"]
mod tests;
