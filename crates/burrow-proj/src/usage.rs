//! The machine out of Claude usage. When Claude Code refuses a run because
//! the plan's usage is spent (a monthly spend limit, a weekly or five-hour
//! limit), every other run is refused the same way until it resets: from
//! 2026-11-01 to 11-04 every job's turn ended at once, 1,391 of them, each
//! retried and reported as an error, the cause named nowhere. So a refused
//! run ([`refused`]) puts the whole machine out of usage until the limit
//! resets ([`record`], in jobs/.usage.json): no turn takes a slot meanwhile
//! (slot.rs), the turn that was refused waits in line again and runs once
//! more after (turn.rs), the stall watchdog holds what waits on it (`proj
//! usage` tells it), and the owner hears once, with the cause and when it
//! resets. The first run that isn't refused afterwards ends it ([`back`]),
//! and the notice is closed; so does `proj usage back`, for a limit the
//! owner raised before it would have reset.

use std::fs::{self, File};
use std::process::Command;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::Config;
use crate::job::is_error;
use crate::limit;
use crate::{now, swap};

/// Where it's kept, in jobs.
pub(crate) const FILE: &str = ".usage.json";
/// The owner's notice's key.
const NOTICE: &str = "proj-usage";
/// How long it's out when Claude Code doesn't say when the limit resets,
/// or says a time already past: it's tried again then.
const UNSAID: i64 = 3600;

/// The machine out of usage: until when, Claude Code's own words, and the
/// job and time it was refused at.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Spent {
    pub until: i64,
    pub said: String,
    pub job: String,
    pub at: i64,
}

/// Whether a run ended refused for usage, and if so when Claude Code says
/// the limit resets (None when it doesn't) and its words. `last` is the
/// run's result; `stream` its lines, where the reset time is as a
/// timestamp; failing that, its words say it ([`crate::limit`]), read at
/// `now`.
pub fn refused(last: &Value, stream: &str, now: i64) -> Option<(Option<i64>, String)> {
    if !is_error(last) {
        return None;
    }
    let said = last["result"].as_str().unwrap_or_default().trim();
    if last["api_error"] != "usage_limit_reached" && !limit_words(said) {
        return None;
    }
    let mut until = None;
    for v in stream
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
    {
        for info in [
            &v["api_error_params"]["rate_limit_info"],
            &v["rate_limit_info"],
        ] {
            if info["status"] == "rejected"
                && let Some(t) = info["resetsAt"].as_i64()
            {
                until = until.max(Some(t));
            }
        }
    }
    // Claude Code's older answer gives its reset after a bar:
    // "Claude AI usage limit reached|1730000000".
    if until.is_none()
        && let Some((_, t)) = said.rsplit_once('|')
    {
        until = t.trim().parse().ok();
    }
    Some((until.or_else(|| limit::resets(said, now)), said.to_owned()))
}

/// Claude Code's words for a spent plan, when its result doesn't carry the
/// API's error: "You've hit your monthly spend limit", "Claude AI usage limit
/// reached", "5-hour limit reached ∙ resets 3pm", "You've reached your usage
/// limit".
fn limit_words(said: &str) -> bool {
    let s = said.to_lowercase();
    s.contains("limit reached")
        || ((s.starts_with("you've hit your") || s.starts_with("you've reached your"))
            && s.contains("limit"))
}

/// The machine's state as it stands: Some while it's out of usage.
pub fn out(cfg: &Config) -> Option<Spent> {
    kept(cfg).filter(|s| s.until > now())
}

fn kept(cfg: &Config) -> Option<Spent> {
    let text = fs::read_to_string(cfg.jobs.join(FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Job `id`'s run was refused at `at`: the machine is out of usage until
/// `until`, or UNSAID on when that's unsaid or past. The owner is told when
/// it wasn't out already. What it's out until, as recorded.
pub fn record(cfg: &Config, id: &str, until: Option<i64>, said: &str, at: i64) -> Spent {
    let until = until.filter(|&t| t > at).unwrap_or(at + UNSAID);
    // Every running turn is refused at once: one of them tells the owner.
    let _lock = File::create(cfg.jobs.join(".usage.lock")).and_then(|f| f.lock().map(|()| f));
    let was = out(cfg);
    let spent = Spent {
        until: was.as_ref().map_or(until, |w| w.until.max(until)),
        said: said.to_owned(),
        job: id.to_owned(),
        at: was.as_ref().map_or(at, |w| w.at),
    };
    if was.as_ref() == Some(&spent) {
        return spent;
    }
    let text = serde_json::to_string(&spent).unwrap_or_default();
    if let Err(e) = swap(&cfg.jobs.join(FILE), &format!("{text}\n")) {
        eprintln!("proj: {FILE}: {e}");
    }
    if was.is_none() {
        tell(cfg, &["open", NOTICE, &title(&spent), &body(&spent)]);
    }
    spent
}

/// A run that wasn't refused, or `proj usage back` (the owner raised the
/// limit): usage is back, the turns waiting for it take slots again, and the
/// owner's notice is closed.
pub fn back(cfg: &Config) {
    let Some(spent) = kept(cfg) else { return };
    if fs::remove_file(cfg.jobs.join(FILE)).is_ok() {
        eprintln!(
            "proj: Claude has usage again (out since {})",
            when(spent.at)
        );
        tell(
            cfg,
            &[
                "close",
                NOTICE,
                "Claude has usage again, and proj's jobs are running.",
            ],
        );
    }
}

/// It, in a line: `proj status`'s, and the turn's journal's.
pub fn line(s: &Spent) -> String {
    format!(
        "Claude is out of usage until {}: no turn starts till then (\"{}\")",
        when(s.until),
        s.said
    )
}

fn title(s: &Spent) -> String {
    format!("Claude is out of usage until {}", when(s.until))
}

fn body(s: &Spent) -> String {
    format!(
        "Since {}, Claude Code refuses every Claude job here. It answered job {}:\n\n> {}\n\n\
         proj starts no turn until {}. The turns that were refused wait in line and run again then, by themselves, \
         and the stall watchdog holds what waits on them. Raised the limit sooner? `proj usage back` starts them now. \
         This closes once a job runs again.",
        when(s.at),
        s.job,
        s.said,
        when(s.until)
    )
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
fn tell(cfg: &Config, args: &[&str]) {
    let Some(notify) = &cfg.notify else {
        eprintln!("proj: {}", args.join(": "));
        return;
    };
    if let Err(e) = Command::new(notify).args(args).status() {
        eprintln!("proj: {}: {e}", notify.display());
    }
}

#[cfg(test)]
mod tests;
