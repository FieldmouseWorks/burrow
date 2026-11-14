//! Find a FLOCK in the kernel's lock table without trusting one read of it.
use std::fs;
use std::io;
use std::path::Path;

/// Reads of the lock table a lock file still taken gets before what it looks
/// for counts as unlisted: each read misses it only while other locks come
/// and go.
const READS: usize = 8;

/// The first line of /proc/locks, split into fields, that `matches` takes,
/// for a lock on `file`. /proc/locks comes a page per read(), each page
/// restarting the kernel's walk by position, so a lock taken or let go
/// elsewhere between pages can shift an entry there all along out of a read.
/// Every entry on `file` (its holder's, or a `->` waiter's) is there only
/// while `file` is taken: while it still is, read the table again.
pub(crate) fn listed(
    file: &Path,
    matches: impl FnMut(&[&str]) -> bool,
) -> io::Result<Option<String>> {
    listed_at(Path::new("/proc/locks"), matches, || {
        super::lock(file) == super::Lock::Taken
    })
}

fn listed_at(
    locks: &Path,
    mut matches: impl FnMut(&[&str]) -> bool,
    mut taken: impl FnMut() -> bool,
) -> io::Result<Option<String>> {
    for _ in 0..READS {
        let table = fs::read_to_string(locks)?;
        let found = table
            .lines()
            .find(|line| matches(&line.split_whitespace().collect::<Vec<_>>()));
        if found.is_some() || !taken() {
            return Ok(found.map(str::to_owned));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OTHER: &str = "1: FLOCK  ADVISORY  WRITE 7 00:01:1 0 EOF\n";
    const WAITER: &str = "1: -> FLOCK  ADVISORY  WRITE 42 00:01:9 0 EOF\n";

    fn waiter(fields: &[&str]) -> bool {
        fields.get(1) == Some(&"->") && fields.get(5) == Some(&"42")
    }

    #[test]
    fn an_entry_a_read_skipped_is_found_while_its_file_is_taken() {
        let tmp = tempfile::tempdir().unwrap();
        let locks = tmp.path().join("locks");
        // The first read came while other locks moved across a page edge.
        fs::write(&locks, OTHER).unwrap();
        let mut asked = 0;
        let found = listed_at(&locks, waiter, || {
            asked += 1;
            fs::write(&locks, format!("{OTHER}{WAITER}")).unwrap();
            true
        })
        .unwrap();
        assert!(found.is_some_and(|line| line.contains("-> FLOCK")));
        assert_eq!(asked, 1);
    }

    #[test]
    fn a_file_let_go_is_read_once_and_one_never_listed_ends() {
        let tmp = tempfile::tempdir().unwrap();
        let locks = tmp.path().join("locks");
        fs::write(&locks, OTHER).unwrap();
        let mut asked = 0;
        let found = listed_at(&locks, waiter, || {
            asked += 1;
            false
        });
        assert_eq!(found.unwrap(), None);
        assert_eq!(asked, 1);
        let mut asked = 0;
        let found = listed_at(&locks, waiter, || {
            asked += 1;
            true
        });
        assert_eq!(found.unwrap(), None);
        assert_eq!(asked, READS);
        // Listed at once: the file isn't looked at.
        fs::write(&locks, WAITER).unwrap();
        let found = listed_at(&locks, waiter, || unreachable!());
        assert!(found.unwrap().is_some());
    }
}
