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
    at(Path::new("/proc"), path)
}

fn at(proc: &Path, path: &Path) -> Option<Owner> {
    let m = fs::metadata(path).ok()?;
    let want = format!(
        "{:02x}:{:02x}:{}",
        rustix::fs::major(m.dev()),
        rustix::fs::minor(m.dev()),
        m.ino()
    );
    let locks = fs::read_to_string(proc.join("locks")).ok()?;
    let (pid, lock) = locks.lines().find_map(|l| {
        let f: Vec<_> = l.split_whitespace().collect();
        (f.get(1) == Some(&"FLOCK") && f.get(5) == Some(&want.as_str()))
            .then(|| (f[4].to_owned(), f[1..].join(" ")))
    })?;
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
        assert_eq!(at(&proc, &path), None);
        // Merely opening the same file is insufficient too.
        fs::write(dir.join("fdinfo/9"), "pos:\t0\nflags:\t0100000\n").unwrap();
        assert_eq!(at(&proc, &path), None);
        fs::write(dir.join("fdinfo/9"), format!("lock:\t1: {lock}\n")).unwrap();
        let owner = at(&proc, &path).unwrap();
        assert_eq!(owner.job, "unrelated");
        fs::write(
            dir.join("stat"),
            format!("42 (proj) {} 12346", ["0"; 19].join(" ")),
        )
        .unwrap();
        assert_ne!(at(&proc, &path), Some(owner));
        fs::write(
            dir.join("fdinfo/9"),
            format!("lock:\t1: {}\n", lock.replace("42 ", "43 ")),
        )
        .unwrap();
        assert_eq!(at(&proc, &path), None);
    }
}
