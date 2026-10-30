//! burrow-events: what happens on GitHub, or on this machine, starts the work
//! it calls for within seconds (modules/events.nix runs it). Nothing waits for
//! a timer to come round.
//!
//! Each handler (the steward, proj-watch, proj-inbox, HOST-update, Renovate)
//! names the GitHub events it cares about and a systemd unit: UNIT.service is
//! its full pass, and UNIT@OWNER-REPO.service its pass over one repo. A
//! handler with no pass over one repo (`perRepo` off) gets its full pass for a
//! repo's event. Three things put a repo in a handler's line:
//!
//!   - A webhook delivery from GitHub (the steward keeps each repo's hook)
//!     whose X-GitHub-Event is one of the handler's. Only a delivery signed
//!     with the hook's secret counts, and all it says is which repo changed:
//!     the pass reads the rest from GitHub. A handler can take only the repos
//!     it names (`repos`), only pushes to a repo's default branch (`pushes`),
//!     only the other events about some branches (`branches`), and only
//!     `issues` events about an issue whose title matches one of some globs
//!     (`issueTitles`).
//!   - A kick on the local port: POST /kick/HANDLER/OWNER/REPO for one repo,
//!     or /kick/HANDLER for a full pass. Something on this machine that just
//!     changed what a pass would find kicks it: a proj job's turn starting or
//!     ending, a review coming back. Kicks only start a pass, which reads
//!     everything for itself, so the port asks for no secret; it listens on
//!     127.0.0.1 and nginx never passes anything to it.
//!   - A wake-up the handler's last pass booked in its wake file,
//!     {repo: epoch} ("" for its full pass), for when a wait it saw ends (a
//!     new head's settle delay, a reviewer's timeout, a reply old enough to
//!     hand on). Read after each pass the service starts, and when it starts.
//!
//! A repo's pass starts DEBOUNCE seconds after its first event, so a push and
//! the events that follow it get one pass, and one repo's passes are GAP
//! apart. A handler's passes run one at a time. When the service starts,
//! every handler gets a full pass, which catches up on whatever happened
//! while it was down.

pub mod glob;
pub mod handler;
pub mod http;
pub mod line;
pub mod passes;
pub mod routes;

use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{SystemTime, UNIX_EPOCH};

/// The time now, in seconds since the epoch, as the wake files have it.
pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Wait for any of these threads, by name, to end (in a panic: none of them
/// returns), and say which. The service is its listeners and its handlers'
/// lines, so one gone is the service failed: systemd starts it again,
/// rather than it running on deaf to a port or a handler.
pub fn supervise(threads: Vec<(String, JoinHandle<()>)>) -> String {
    let (ended, gone) = mpsc::channel();
    for (name, t) in threads {
        let ended = ended.clone();
        thread::spawn(move || {
            let _ = t.join();
            let _ = ended.send(name);
        });
    }
    drop(ended);
    match gone.recv() {
        Ok(name) => name,
        // None to wait for: nothing ends.
        Err(_) => loop {
            thread::park();
        },
    }
}

/// OWNER/REPO as GitHub has them, and never one that reads as an option.
pub fn repo_name(s: &str) -> bool {
    let Some((owner, name)) = s.split_once('/') else {
        return false;
    };
    let mut o = owner.chars();
    let mut n = name.chars();
    o.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && o.all(|c| c.is_ascii_alphanumeric() || c == '-')
        && n.next()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
        && n.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use super::{repo_name, supervise};

    // The first thread to end is the one named, however it ends.
    #[test]
    fn supervises() {
        let forever = thread::spawn(|| {
            loop {
                thread::park();
            }
        });
        let dies = thread::spawn(|| {
            thread::sleep(Duration::from_millis(50));
            panic!("a bug");
        });
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            tx.send(supervise(vec![
                ("kicks".into(), forever),
                ("autoland".into(), dies),
            ]))
        });
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(10)).as_deref(),
            Ok("autoland")
        );
    }

    #[test]
    fn repo_names() {
        for good in ["sam/r", "Sam-2/my.repo_x", "a/.github", "a/_b-c"] {
            assert!(repo_name(good), "{good}");
        }
        for bad in [
            "--help", "--help/x", "sam", "sam/", "/r", "sam/r/x", "sam/-r", "-sam/r", "sa_m/r",
            "sam/r\n", "sam /r", "",
        ] {
            assert!(!repo_name(bad), "{bad:?}");
        }
    }
}
