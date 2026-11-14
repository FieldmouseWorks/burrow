//! What the owner and `proj status` are told of a hold: its notice through
//! HOST-notify, opened and closed, and its line.

use std::process::{Command, Stdio};

use jiff::Timestamp;
use jiff::tz::TimeZone;

use super::{NOTICE, Spent, login};
use crate::config::Config;

pub(super) const CLOSED: &str = "Claude has usage again, and proj's jobs are running.";

/// What closes its notice.
pub(super) fn closed(s: &Spent) -> &'static str {
    if s.login { login::CLOSED } else { CLOSED }
}

/// Whether HOST-notify may still have the notice open: a close that didn't
/// get through. Only its exit 1 says it's closed (or was never opened); one
/// that couldn't run, was killed or answered anything else may still have
/// it. Without HOST-notify, nothing is.
pub(super) fn pending(cfg: &Config) -> bool {
    cfg.notify.as_ref().is_some_and(|n| {
        !Command::new(n)
            .args(["pending", NOTICE])
            .stdout(Stdio::null())
            .status()
            .is_ok_and(|st| st.code() == Some(1))
    })
}

/// It, in a line: `proj status`'s, and the turn's journal's.
pub fn line(s: &Spent) -> String {
    hold_line(s.owner, s.login, s.until, &s.said)
}

/// A hold in a line, from what the live snapshot keeps of it: the owner's
/// pause or a refusal (of usage, or of the `login`), until when, and its
/// reason or Claude Code's words.
pub fn hold_line(owner: bool, login: bool, until: i64, said: &str) -> String {
    if login {
        return login::line(until, said);
    }
    if owner {
        return format!(
            "Claude work is paused by the owner until {}: {said}",
            when(until)
        );
    }
    format!(
        "Claude is out of usage until {}: no Claude turn starts till then (\"{said}\")",
        when(until)
    )
}

pub(super) fn title(s: &Spent) -> String {
    if s.login {
        return login::title(s);
    }
    if s.owner {
        return format!("Claude work paused by the owner until {}", when(s.until));
    }
    format!("Claude is out of usage until {}", when(s.until))
}

pub(super) fn body(cfg: &Config, s: &Spent) -> String {
    if s.login {
        return login::body(s);
    }
    if s.owner {
        return format!(
            "The owner paused Claude work until {}. Reason: {}. Running turns continue; Codex work remains eligible. Use `proj usage back` to release this pause early.",
            when(s.until),
            s.said
        );
    }
    format!(
        "Since {}, Claude Code refuses every Claude job here. It answered job {}:\n\n> {}\n\n\
         proj starts no Claude turn until {}. The turns that were refused wait in line and run again then, by themselves, \
         and the stall watchdog holds what waits on them. {} \
         This closes once a job runs again.",
        when(s.at),
        s.job,
        s.said,
        when(s.until),
        policy(cfg)
    )
}

pub fn policy(cfg: &Config) -> &'static str {
    if cfg.usage_paid {
        "If usage is available sooner, `proj usage back` releases the hold."
    } else {
        "Paid extra usage is disabled. A usage limit means waiting until reset; never ask the owner to raise a spend cap or enable paid usage."
    }
}

/// A moment as the machine's clock (its time zone) says it.
pub fn when(t: i64) -> String {
    Timestamp::from_second(t).map_or_else(
        |_| t.to_string(),
        |at| {
            at.to_zoned(TimeZone::system())
                .strftime("%a %b %-d, %-I:%M %p %Z")
                .to_string()
        },
    )
}

/// HOST-notify, which never fails its caller; without it, the journal has
/// it.
pub(super) fn tell(cfg: &Config, args: &[&str]) {
    let Some(notify) = &cfg.notify else {
        eprintln!("proj: {}", args.join(": "));
        return;
    };
    if let Err(e) = Command::new(notify).args(args).status() {
        eprintln!("proj: {}: {e}", notify.display());
    }
}
