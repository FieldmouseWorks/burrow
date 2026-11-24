//! Per-engine owner notices (`HOST-notify`) and human-readable hold descriptions.

use std::process::{Command, Stdio};

use jiff::Timestamp;
use jiff::tz::TimeZone;

use crate::config::Config;

use super::login;
use super::store::{ALL_ENGINES, DEFAULT_ENGINE, Spent};

/// The owner notice key for a hold: `proj-usage` for global owner pauses (`*`),
/// and `proj-usage-<engine>` for engine-specific holds so one engine's notice
/// never collides with or closes another's.
pub(crate) fn key(engine: &str) -> String {
    if engine == ALL_ENGINES {
        "proj-usage".to_owned()
    } else {
        format!("proj-usage-{engine}")
    }
}

/// Human-facing label for an engine key in notices and status lines.
pub(crate) fn display_name(engine: &str) -> &str {
    match engine {
        DEFAULT_ENGINE => "Claude",
        ALL_ENGINES => "All engines",
        other => other,
    }
}

/// Whether `HOST-notify` may still have `engine`'s notice open.
pub(crate) fn pending(cfg: &Config, engine: &str) -> bool {
    let notice = key(engine);
    cfg.notify.as_ref().is_some_and(|n| {
        !Command::new(n)
            .args(["pending", &notice])
            .stdout(Stdio::null())
            .status()
            .is_ok_and(|st| st.code() == Some(1))
    })
}

/// A hold in a single line (for `proj status` and turn journals).
pub fn line(s: &Spent) -> String {
    let engine = if s.engine.is_empty() {
        DEFAULT_ENGINE
    } else {
        s.engine.as_str()
    };
    hold_line_kind(engine, s.owner, s.is_login(), s.until, &s.said)
}

/// A hold in a single line from snapshot fields (`engine`, `owner`, `until`, `said`).
pub fn hold_line_for(engine: &str, owner: bool, until: i64, said: &str) -> String {
    hold_line_kind(engine, owner, !owner && login::is(said), until, said)
}

/// Convenience wrapper for the default (`claude`) hold line.
pub fn hold_line(owner: bool, until: i64, said: &str) -> String {
    hold_line_for(DEFAULT_ENGINE, owner, until, said)
}

fn hold_line_kind(engine: &str, owner: bool, is_login: bool, until: i64, said: &str) -> String {
    let label = display_name(engine);
    if owner {
        if engine == ALL_ENGINES {
            return format!(
                "All engines are paused by the owner until {}: {said}",
                when(until)
            );
        }
        return format!(
            "{label} work is paused by the owner until {}: {said}",
            when(until)
        );
    }
    if is_login {
        return login::line_for(engine, until);
    }
    format!(
        "{label} is out of usage until {}: no {label} turn starts till then (\"{said}\")",
        when(until)
    )
}

pub(crate) fn title(s: &Spent) -> String {
    let engine = if s.engine.is_empty() {
        DEFAULT_ENGINE
    } else {
        s.engine.as_str()
    };
    let label = display_name(engine);
    if s.owner {
        if engine == ALL_ENGINES {
            return format!("All engines paused by the owner until {}", when(s.until));
        }
        return format!("{label} work paused by the owner until {}", when(s.until));
    }
    if s.is_login() {
        return login::title_for(engine);
    }
    format!("{label} is out of usage until {}", when(s.until))
}

pub(crate) fn body(cfg: &Config, s: &Spent) -> String {
    let engine = if s.engine.is_empty() {
        DEFAULT_ENGINE
    } else {
        s.engine.as_str()
    };
    let label = display_name(engine);
    if s.owner {
        let scope = if engine == ALL_ENGINES {
            "all engines".to_owned()
        } else {
            format!("{label} work")
        };
        let back_cmd = if engine == ALL_ENGINES {
            "proj usage back".to_owned()
        } else {
            format!("proj usage back --engine {engine}")
        };
        return format!(
            "The owner paused {scope} until {}. Reason: {}. Running turns continue. Use `{back_cmd}` to release this pause early.",
            when(s.until),
            s.said
        );
    }
    if s.is_login() {
        return login::body_for(engine, s);
    }
    let harness = if engine == DEFAULT_ENGINE {
        "Claude Code"
    } else {
        engine
    };
    format!(
        "Since {}, {harness} refuses every {label} job here. It answered job {}:\n\n> {}\n\n\
         proj starts no {label} turn until {}. The turns that were refused wait in line and run again then, by themselves, \
         and the stall watchdog holds what waits on them. {} \
         This closes once a {label} job runs again.",
        when(s.at),
        s.job,
        s.said,
        when(s.until),
        policy(cfg)
    )
}

pub(crate) fn closed(s: &Spent) -> String {
    let engine = if s.engine.is_empty() {
        DEFAULT_ENGINE
    } else {
        s.engine.as_str()
    };
    if s.is_login() {
        return login::closed_for(engine);
    }
    if engine == ALL_ENGINES {
        return "All engines are unpaused, and proj's jobs are running.".to_owned();
    }
    let label = display_name(engine);
    format!("{label} has usage again, and proj's jobs are running.")
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
            at.to_zoned_datetime(TimeZone::system())
                .strftime("%a %b %-d, %-I:%M %p %Z")
                .to_string()
        },
    )
}

/// Invoke `HOST-notify` without failing its caller; log to stderr if unconfigured.
pub(crate) fn tell(cfg: &Config, args: &[&str]) {
    let Some(notify) = &cfg.notify else {
        eprintln!("proj: {}", args.join(": "));
        return;
    };
    if let Err(e) = Command::new(notify).args(args).status() {
        eprintln!("proj: {}: {e}", notify.display());
    }
}
