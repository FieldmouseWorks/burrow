//! The decisions, with no I/O.

/// What a `.git` in a folder is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Git {
    /// A folder: a checkout.
    Dir,
    /// Nothing there.
    Absent,
    /// A file (a linked worktree's) or anything else.
    Other,
}

/// What a folder in the projects folder is to the scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Folder {
    /// Gets a server (claude-rc@NAME).
    Project,
    /// Not one yet, but may become one where it stands: watched for its
    /// origin (claude-rc-origin@NAME).
    Waiting,
    /// Never one: gets neither.
    Neither,
}

/// A project is a git checkout with an origin. Its .git is a folder: a
/// linked worktree's is a file, and it's a checkout of a project that has
/// its own server. Anything else would be a server with nothing to push to.
/// The machine's own flake is served whatever it is.
///
/// A folder that isn't one yet may become one where it stands, its origin
/// written whenever that comes: a clone's a moment after it makes the
/// folder, or after a pause, or `git init` and a remote added by hand.
/// Nothing else changes in the projects folder then, so each such folder has
/// a path unit on its .git/config while it isn't a project, which scans
/// again when that is written. A linked worktree never becomes one, and gets
/// none.
///
/// `has_origin` is asked only when it can matter.
pub fn folder(is_flake: bool, git: Git, has_origin: impl FnOnce() -> bool) -> Folder {
    if is_flake || (git == Git::Dir && has_origin()) {
        Folder::Project
    } else if matches!(git, Git::Dir | Git::Absent) {
        Folder::Waiting
    } else {
        Folder::Neither
    }
}

/// Whether a session holds its server, and until when at the soonest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hold {
    pub held: bool,
    /// In milliseconds since the epoch. Only means something when held.
    pub until: i64,
}

/// Whether a session holds its server: it is working (busy), or did
/// something within the quiet spell (`quiet` ms before `now`). `status` and
/// `at` are what `jq` read from Claude Code's ~/.claude/sessions/PID.json
/// (its `.status`, and `.statusUpdatedAt` in ms, 0 without one), or nothing
/// where it couldn't. A session that says nothing, or whose file can't be
/// made sense of, is held for a quiet spell from now: it may end without a
/// word.
pub fn hold(status: Option<&str>, at: Option<&str>, now: i64, quiet: i64) -> Hold {
    let fresh = Hold {
        held: true,
        until: now + quiet,
    };
    let (Some(status), Some(at)) = (status, at) else {
        return fresh;
    };
    if status == "busy" {
        return fresh;
    }
    // A time that isn't one is no evidence of a quiet session: the shell
    // stopped on it, leaving the server be, and so is held a spell from now.
    let at = at.trim();
    let Some(at) = at.parse::<i64>().ok().or_else(|| {
        at.parse::<f64>()
            .ok()
            .filter(|f| f.is_finite() && f.abs() < 9e15)
            .map(|f| f as i64)
    }) else {
        return fresh;
    };
    let until = at + quiet;
    Hold {
        held: now < until,
        until,
    }
}

/// What a stale server comes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Server {
    /// No session holds it: restart it now.
    Restart,
    /// Held until then, at the latest of its sessions' spells.
    Held(i64),
}

pub fn server(sessions: impl IntoIterator<Item = Hold>) -> Server {
    let mut last = None;
    for hold in sessions.into_iter().filter(|h| h.held) {
        last = Some(last.map_or(hold.until, |l: i64| l.max(hold.until)));
    }
    // A held spell is never before the epoch's start, as `last=0` was.
    last.map_or(Server::Restart, |l| Server::Held(l.max(0)))
}

/// The wake-up: the soonest of the held servers' ends.
pub fn wake(servers: impl IntoIterator<Item = Server>) -> Option<i64> {
    servers
        .into_iter()
        .filter_map(|s| match s {
            Server::Held(until) => Some(until),
            Server::Restart => None,
        })
        .min()
}

/// How long until the wake, from the clock now: none if it is over already,
/// which is the look after this scan's to see.
pub fn left(wake: i64, now: i64) -> Option<i64> {
    (wake > now).then(|| wake - now)
}

/// Whether a server's command line (`/proc/PID/cmdline`) is a current
/// one: the tmux and claude-rc-serve it was started with.
pub fn current(cmdline: &[u8], tmux: &str, serve: &str) -> bool {
    let line: Vec<u8> = cmdline
        .iter()
        .map(|&b| if b == 0 { b' ' } else { b })
        .collect();
    let mut prefix = tmux.as_bytes().to_vec();
    prefix.push(b' ');
    let serve = serve.as_bytes();
    line.strip_prefix(prefix.as_slice())
        .is_some_and(|rest| rest.windows(serve.len()).any(|w| w == serve))
}

/// Whether a process state (`/proc/PID/stat`) is a live session's: any
/// but Z, a zombie being a session that already ended.
pub fn live(state: char) -> bool {
    matches!(state, 'D' | 'I' | 'R' | 'S' | 'T' | 't')
}

#[cfg(test)]
mod tests {
    use super::*;

    const QUIET: i64 = 30 * 60_000;
    const NOW: i64 = 1_700_000_000_000;

    #[test]
    fn a_checkout_with_an_origin_is_a_project() {
        assert_eq!(folder(false, Git::Dir, || true), Folder::Project);
    }

    #[test]
    fn a_checkout_without_one_waits_for_it() {
        assert_eq!(folder(false, Git::Dir, || false), Folder::Waiting);
    }

    #[test]
    fn a_folder_without_git_may_become_one() {
        assert_eq!(
            folder(false, Git::Absent, || panic!("no .git to ask")),
            Folder::Waiting
        );
    }

    #[test]
    fn a_linked_worktree_is_never_one() {
        assert_eq!(
            folder(false, Git::Other, || panic!("not a checkout")),
            Folder::Neither
        );
    }

    #[test]
    fn the_flake_is_served_whatever_it_is() {
        for git in [Git::Dir, Git::Absent, Git::Other] {
            assert_eq!(folder(true, git, || false), Folder::Project);
        }
    }

    fn held(status: &str, at: i64) -> Hold {
        hold(Some(status), Some(&at.to_string()), NOW, QUIET)
    }

    #[test]
    fn a_working_session_holds() {
        assert_eq!(
            held("busy", NOW - 10 * QUIET),
            Hold {
                held: true,
                until: NOW + QUIET
            }
        );
    }

    #[test]
    fn a_session_active_in_the_quiet_spell_holds_until_it_ends() {
        let at = NOW - 60_000;
        assert_eq!(
            held("idle", at),
            Hold {
                held: true,
                until: at + QUIET
            }
        );
    }

    #[test]
    fn a_session_quiet_for_the_whole_spell_does_not_hold() {
        // The spell's last instant is over: held only while now < until.
        assert!(!held("idle", NOW - QUIET).held);
        assert!(held("idle", NOW - QUIET + 1).held);
    }

    #[test]
    fn a_session_that_says_nothing_holds_a_spell_from_now() {
        let fresh = Hold {
            held: true,
            until: NOW + QUIET,
        };
        assert_eq!(hold(None, None, NOW, QUIET), fresh);
        assert_eq!(hold(Some("idle"), None, NOW, QUIET), fresh);
        assert_eq!(hold(None, Some("0"), NOW, QUIET), fresh);
    }

    #[test]
    fn a_session_with_no_time_is_quiet() {
        assert!(!hold(Some("idle"), Some("0"), NOW, QUIET).held);
        assert_eq!(
            hold(Some("idle"), Some("1.5e12"), NOW, QUIET).until,
            1_500_000_000_000 + QUIET
        );
    }

    #[test]
    fn a_time_that_is_not_one_is_not_a_quiet_session() {
        let fresh = Hold {
            held: true,
            until: NOW + QUIET,
        };
        for bad in ["not a time", "", "NaN", "inf", "1e400"] {
            assert_eq!(hold(Some("idle"), Some(bad), NOW, QUIET), fresh, "{bad}");
        }
    }

    #[test]
    fn a_server_nothing_holds_restarts() {
        assert_eq!(server([]), Server::Restart);
        assert_eq!(
            server([Hold {
                held: false,
                until: NOW + 5
            }]),
            Server::Restart
        );
    }

    #[test]
    fn a_held_server_waits_for_its_latest_session() {
        let hs = [
            Hold {
                held: true,
                until: 300,
            },
            Hold {
                held: false,
                until: 900,
            },
            Hold {
                held: true,
                until: 500,
            },
        ];
        assert_eq!(server(hs), Server::Held(500));
    }

    #[test]
    fn the_wake_is_the_soonest_server() {
        assert_eq!(
            wake([Server::Held(500), Server::Restart, Server::Held(300)]),
            Some(300)
        );
        assert_eq!(wake([Server::Restart]), None);
        assert_eq!(wake([]), None);
    }

    #[test]
    fn a_wake_over_already_books_nothing() {
        assert_eq!(left(1_500, 1_000), Some(500));
        assert_eq!(left(1_000, 1_000), None);
        assert_eq!(left(900, 1_000), None);
    }

    #[test]
    fn a_server_is_current_on_its_tmux_and_serve() {
        let line = b"/t/tmux\0-L\0rc-x\0new-session\0sh -c exec /s/bin/claude-rc-serve\0";
        assert!(current(line, "/t/tmux", "/s/bin/claude-rc-serve"));
        // Another claude-rc-serve, or another tmux.
        assert!(!current(line, "/t/tmux", "/other/bin/claude-rc-serve"));
        assert!(!current(line, "/u/tmux", "/s/bin/claude-rc-serve"));
        // The tmux must lead, as a whole word.
        assert!(!current(
            b"/t/tmuxer -L /s/bin/claude-rc-serve",
            "/t/tmux",
            "/s/bin/claude-rc-serve"
        ));
    }

    #[test]
    fn a_zombie_is_not_a_live_session() {
        assert!(!live('Z'));
        assert!(['D', 'I', 'R', 'S', 'T', 't'].into_iter().all(live));
    }

    /// The proof the tests above catch a broken decision: the spell compared
    /// with `<=` instead of `<` (the bug these tests are for) agrees with
    /// `hold` everywhere but the instant the spell ends, which is where
    /// a_session_quiet_for_the_whole_spell_does_not_hold looks.
    #[test]
    fn the_boundary_test_fails_on_a_spell_that_ends_one_instant_late() {
        let broken = |now: i64, at: i64| now <= at + QUIET;
        assert!(broken(NOW, NOW - QUIET));
        assert!(!held("idle", NOW - QUIET).held);
    }
}
