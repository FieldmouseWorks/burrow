//! A hold for Claude Code's login. From 17:50 to 19:11 on 2026-11-12 its
//! OAuth session couldn't be refreshed, and every Claude turn that ended
//! then ended in error, 148 of them, each left for someone to notice and
//! resume. A run refused that way is held like one out of usage ([`super`]),
//! with what's particular to it here: nothing says when the login is back,
//! so the hold lasts [`RETRY`] and the next run tests it, refused once more
//! (held again) or ending it; and a login that comes back by itself needs
//! no one, so the owner hears only once it has been refused for
//! [`DEADLINE`] with no run getting through.

use super::{RETELL, Spent, when};

/// How long a refused login holds Claude turns before the next run tests
/// it: a refusal costs that run nothing but its start.
pub(super) const RETRY: i64 = 300;

/// How long the login may be refused before the owner is told it needs
/// signing in again, from the first refusal no run has got through since.
pub(super) const DEADLINE: i64 = 1800;

/// What closes the owner's notice.
pub(super) const CLOSED: &str = "Claude Code's login works again, and proj's jobs are running.";

/// Whether the owner has been told of it: since it was first refused.
fn told(s: &Spent) -> bool {
    s.told >= s.at
}

/// Whether the owner's notice is due at `now`: refused past DEADLINE, and
/// not told yet, or told RETELL ago (HOST-notify never says whether GitHub
/// took it; sending it again updates the one notice).
pub(super) fn due(s: &Spent, now: i64) -> bool {
    now - s.at >= DEADLINE && (!told(s) || now - s.told >= RETELL)
}

/// When proj-watch looks at it next while it holds: at DEADLINE, or RETELL
/// after the owner was told.
pub(super) fn next(s: &Spent) -> i64 {
    if told(s) {
        s.told + RETELL
    } else {
        s.at + DEADLINE
    }
}

/// `proj status`'s line for it.
pub(super) fn line(until: i64, said: &str) -> String {
    format!(
        "Claude Code's login was refused: no Claude turn starts till {}, when the next one tests it again (\"{said}\")",
        when(until)
    )
}

pub(super) fn title(_: &Spent) -> String {
    "Claude Code needs signing in again".into()
}

pub(super) fn body(s: &Spent) -> String {
    format!(
        "Since {}, Claude Code's login is refused for every Claude job here. It answered job {}:\n\n> {}\n\n\
         proj starts no Claude turn meanwhile, and tries the login again every {} minutes: the turns that were refused \
         wait in line and run again by themselves once it works, and the stall watchdog holds what waits on them. \
         Signing Claude Code in again, as the user its jobs run as, is yours. This closes once a job runs again.",
        when(s.at),
        s.job,
        s.said,
        RETRY / 60
    )
}
