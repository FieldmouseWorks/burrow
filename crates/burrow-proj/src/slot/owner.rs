//! Match the kernel's acquirer to a live process with the locked description.
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Owner {
    pub(super) job: String,
    pid: String,
    started: String,
    lock: String,
}

pub(super) fn read(path: &Path) -> Option<Owner> {
    at(Path::new("/proc"), path, || {
        super::lock(path) == super::Lock::Taken
    })
}

/// Reads of the lock table a lock still taken gets before it counts as
/// unlisted: each one misses it only while other locks come and go.
const READS: usize = 8;

/// The kernel's acquirer and lock line for the FLOCK on `want`. /proc/locks
/// comes a page per read(), each page restarting the walk by position, so a
/// lock taken or let go elsewhere between pages can shift one held all along
/// out of a read. While the file is still `taken`, its lock is in the table:
/// read it again.
fn listed(proc: &Path, want: &str, mut taken: impl FnMut() -> bool) -> Option<(String, String)> {
    for _ in 0..READS {
        let locks = fs::read_to_string(proc.join("locks")).ok()?;
        let found = locks.lines().find_map(|l| {
            let f: Vec<_> = l.split_whitespace().collect();
            (f.get(1) == Some(&"FLOCK") && f.get(5) == Some(&want))
                .then(|| (f[4].to_owned(), f[1..].join(" ")))
        });
        if found.is_some() || !taken() {
            return found;
        }
    }
    None
}

fn at(proc: &Path, path: &Path, taken: impl FnMut() -> bool) -> Option<Owner> {
    let m = fs::metadata(path).ok()?;
    let want = format!(
        "{:02x}:{:02x}:{}",
        rustix::fs::major(m.dev()),
        rustix::fs::minor(m.dev()),
        m.ino()
    );
    let (pid, lock) = listed(proc, &want, taken)?;
    let dir = proc.join(&pid);
    let started = start(&dir)?;
    let cmdline = fs::read(dir.join("cmdline")).ok()?;
    let args: Vec<_> = cmdline.split(|b| *b == 0).collect();
    let i = args.iter().position(|a| *a == b"_exec")?;
    let job = String::from_utf8(args.get(i + 1)?.to_vec()).ok()?;
    if job.is_empty() || !has_lock(&dir, &lock) || start(&dir)? != started {
        return None;
    }
    Some(Owner {
        job,
        pid,
        started,
        lock,
    })
}

fn start(dir: &Path) -> Option<String> {
    let stat = fs::read_to_string(dir.join("stat")).ok()?;
    // comm can contain spaces and parentheses; field 22 is starttime.
    Some(
        stat.rsplit_once(')')?
            .1
            .split_whitespace()
            .nth(19)?
            .to_owned(),
    )
}

fn has_lock(dir: &Path, lock: &str) -> bool {
    let Ok(fds) = fs::read_dir(dir.join("fdinfo")) else {
        return false;
    };
    fds.filter_map(Result::ok).any(|fd| {
        fs::read_to_string(fd.path()).is_ok_and(|info| {
            info.lines().any(|line| {
                let f: Vec<_> = line.split_whitespace().collect();
                // fdinfo numbers locks per descriptor, /proc/locks per listing.
                f.first() == Some(&"lock:") && f.len() > 2 && f[2..].join(" ") == lock
            })
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reused_pid_needs_the_current_locked_description() {
        let tmp = tempfile::tempdir().unwrap();
        let proc = tmp.path().join("proc");
        let dir = proc.join("42");
        fs::create_dir_all(dir.join("fdinfo")).unwrap();
        let path = tmp.path().join("slot");
        fs::write(&path, "").unwrap();
        let m = fs::metadata(&path).unwrap();
        let lock = format!(
            "FLOCK ADVISORY WRITE 42 {:02x}:{:02x}:{} 0 EOF",
            rustix::fs::major(m.dev()),
            rustix::fs::minor(m.dev()),
            m.ino()
        );
        fs::write(proc.join("locks"), format!("7: {lock}\n")).unwrap();
        fs::write(dir.join("cmdline"), b"proj\0_exec\0unrelated\0normal\0").unwrap();
        fs::write(
            dir.join("stat"),
            format!("42 (proj) {} 12345", ["0"; 19].join(" ")),
        )
        .unwrap();
        // An exited acquirer's PID was reused while its child kept the lock.
        assert_eq!(at(&proc, &path, || false), None);
        // Merely opening the same file is insufficient too.
        fs::write(dir.join("fdinfo/9"), "pos:\t0\nflags:\t0100000\n").unwrap();
        assert_eq!(at(&proc, &path, || false), None);
        fs::write(dir.join("fdinfo/9"), format!("lock:\t1: {lock}\n")).unwrap();
        let owner = at(&proc, &path, || false).unwrap();
        assert_eq!(owner.job, "unrelated");
        fs::write(
            dir.join("stat"),
            format!("42 (proj) {} 12346", ["0"; 19].join(" ")),
        )
        .unwrap();
        assert_ne!(at(&proc, &path, || false), Some(owner));
        fs::write(
            dir.join("fdinfo/9"),
            format!("lock:\t1: {}\n", lock.replace("42 ", "43 ")),
        )
        .unwrap();
        assert_eq!(at(&proc, &path, || false), None);
    }

    #[test]
    fn a_lock_still_taken_is_read_again_when_a_read_skipped_it() {
        let tmp = tempfile::tempdir().unwrap();
        let proc = tmp.path().join("proc");
        let dir = proc.join("42");
        fs::create_dir_all(dir.join("fdinfo")).unwrap();
        let path = tmp.path().join("slot");
        fs::write(&path, "").unwrap();
        let m = fs::metadata(&path).unwrap();
        let lock = format!(
            "FLOCK ADVISORY WRITE 42 {:02x}:{:02x}:{} 0 EOF",
            rustix::fs::major(m.dev()),
            rustix::fs::minor(m.dev()),
            m.ino()
        );
        fs::write(dir.join("cmdline"), b"proj\0_exec\0held\0normal\0").unwrap();
        fs::write(
            dir.join("stat"),
            format!("42 (proj) {} 12345", ["0"; 19].join(" ")),
        )
        .unwrap();
        fs::write(dir.join("fdinfo/9"), format!("lock:\t1: {lock}\n")).unwrap();
        let other = "1: FLOCK ADVISORY WRITE 7 00:01:1 0 EOF\n";
        // The first read came while other locks moved across a page edge.
        fs::write(proc.join("locks"), other).unwrap();
        let mut asked = 0;
        let owner = at(&proc, &path, || {
            asked += 1;
            fs::write(proc.join("locks"), format!("{other}2: {lock}\n")).unwrap();
            true
        });
        assert_eq!(owner.map(|o| o.job), Some("held".into()));
        assert_eq!(asked, 1);
        // Let go meanwhile: one read, and no owner.
        fs::write(proc.join("locks"), other).unwrap();
        let mut asked = 0;
        assert_eq!(
            at(&proc, &path, || {
                asked += 1;
                false
            }),
            None
        );
        assert_eq!(asked, 1);
        // Taken but never listed (another kind of lock): the reads end.
        let mut asked = 0;
        assert_eq!(
            at(&proc, &path, || {
                asked += 1;
                true
            }),
            None
        );
        assert_eq!(asked, READS);
    }
}
