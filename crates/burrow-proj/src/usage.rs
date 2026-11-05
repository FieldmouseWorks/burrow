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
//! resets. The first run that started after the refusal and isn't refused
//! ends it ([`back`]), and the notice is closed; so does `proj usage back`,
//! for a limit the owner raised before it would have reset.
//!
//! Every change to it goes under jobs/.usage.lock, so turns refused at once,
//! and runs that finish meanwhile, each see the last one's. Its record
//! outlives the hold: until the owner's notice is surely closed, and for a
//! stall's time after the line moved again, which the watchdog's deadlines
//! for turns in line count from ([`Spent::release`]). HOST-notify can't say
//! whether GitHub took a notice, so the open is sent again every RETELL
//! while it holds ([`remind`]), and the close until `HOST-notify pending`
//! says it got through: by the runs, and by proj-watch's every look, which
//! books its next for it ([`tend`]), so neither waits on a run to come.

use std::fs::{self, File};
use std::process::{Command, Stdio};

use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::Config;
use crate::job::{Job, is_error, kick};
use crate::limit;
use crate::waits::rewatch_all;
use crate::{now, swap};

/// Where it's kept, in jobs.
pub(crate) const FILE: &str = ".usage.json";
/// The owner's notice's key.
const NOTICE: &str = "proj-usage";
/// How long it's out when Claude Code doesn't say when the limit resets,
/// or says a time already past: it's tried again then.
const UNSAID: i64 = 3600;
/// How often the owner's notice is sent again while it may not have got
/// through: HOST-notify never says.
const RETELL: i64 = 3600;

/// The machine out of usage: until when, Claude Code's own words, and the
/// job and time it was refused at.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Spent {
    pub until: i64,
    pub said: String,
    pub job: String,
    pub at: i64,
    /// When it ended, by a run that wasn't refused or `proj usage back`;
    /// None while it holds.
    #[serde(default)]
    pub back: Option<i64>,
    /// When the owner's notice was last sent: its open while it holds, its
    /// close after.
    #[serde(default)]
    pub told: i64,
    /// When a run was last refused while it held: only a run that started
    /// after that says usage is back ([`back`]). 0 in a record from before
    /// it was kept, which `at` stands for.
    #[serde(default)]
    pub last: i64,
}

impl Spent {
    /// When the line moved again: its reset, or sooner if it ended sooner.
    pub fn release(&self) -> i64 {
        self.back.map_or(self.until, |b| b.min(self.until))
    }

    /// Whether it holds the line at `now`.
    pub fn holds(&self, now: i64) -> bool {
        self.back.is_none() && self.until > now
    }

    /// When a run was last refused: a run that started then or before
    /// proves nothing of usage now.
    pub fn refused(&self) -> i64 {
        self.last.max(self.at)
    }
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
    kept(cfg).filter(|s| s.holds(now()))
}

/// Its record, holding or not, while it's kept.
pub fn kept(cfg: &Config) -> Option<Spent> {
    let text = fs::read_to_string(cfg.jobs.join(FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

/// jobs/.usage.lock, held: every change to the record goes under it.
fn lock(cfg: &Config) -> Option<File> {
    let f = File::create(cfg.jobs.join(".usage.lock")).ok()?;
    f.lock().ok()?;
    Some(f)
}

fn keep(cfg: &Config, s: &Spent) {
    let text = serde_json::to_string(s).unwrap_or_default();
    if let Err(e) = swap(&cfg.jobs.join(FILE), &format!("{text}\n")) {
        eprintln!("proj: {FILE}: {e}");
    }
}

/// Job `id`'s run was refused at `at`: the machine is out of usage until
/// `until`, or UNSAID on when that's unsaid or past. The owner is told when
/// it wasn't out already, or its reset had passed (a run refused again then
/// is a new one to tell), and every turn in line has its deadline moved.
/// What it's out until, as recorded.
pub fn record(cfg: &Config, id: &str, until: Option<i64>, said: &str, at: i64) -> Spent {
    let until = until.filter(|&t| t > at).unwrap_or(at + UNSAID);
    let _lock = lock(cfg);
    let was = kept(cfg).filter(|w| w.back.is_none());
    let lapsed = was.as_ref().is_none_or(|w| !w.holds(at));
    let spent = match &was {
        // Every running turn is refused at once: the first tells the owner,
        // the rest move the reset on, and when it was last refused.
        Some(w) => Spent {
            until: w.until.max(until),
            last: w.refused().max(at),
            told: if lapsed { now() } else { w.told },
            ..w.clone()
        },
        None => Spent {
            until,
            said: said.to_owned(),
            job: id.to_owned(),
            at,
            back: None,
            told: now(),
            last: at,
        },
    };
    if was.as_ref() == Some(&spent) {
        return spent;
    }
    keep(cfg, &spent);
    if lapsed {
        tell(cfg, &["open", NOTICE, &title(&spent), &body(&spent)]);
    }
    if lapsed || was.as_ref().is_some_and(|w| w.until != spent.until) {
        changed(cfg);
    }
    spent
}

/// The hold changed: every turn in line has its deadline moved, the stall
/// watchdog looks again whether or not those did (it holds every other
/// source's stalls by it too), and so does proj-watch, which tends the
/// owner's notice from then on ([`tend`]).
fn changed(cfg: &Config) {
    rewatch_all(cfg);
    let mut kicks = vec!["proj-watch".to_owned()];
    if cfg.waits.is_some() {
        kicks.push("watchdog".to_owned());
    }
    kick(cfg, kicks);
}

/// While it holds, the owner's notice once more every RETELL: one GitHub
/// didn't take is sent again. Cheap when there's nothing to do, so every
/// turn in line can ask.
pub fn remind(cfg: &Config) {
    let due = |s: &Spent| s.holds(now()) && now() - s.told >= RETELL;
    if !kept(cfg).is_some_and(|s| due(&s)) {
        return;
    }
    let _lock = lock(cfg);
    if let Some(mut s) = kept(cfg).filter(|s| due(s)) {
        s.told = now();
        keep(cfg, &s);
        tell(cfg, &["open", NOTICE, &title(&s), &body(&s)]);
    }
}

/// Usage is back: a run that started at `started` (in seconds) and wasn't
/// refused, or `proj usage back` (None: the owner raised the limit). The
/// turns waiting take slots again, every turn in line has its deadline
/// moved, and the owner's notice is closed. A run that started no later
/// than the last refusal proves nothing: its last call may have come before
/// it. Once it has ended, each call settles what's left ([`settle`]).
pub fn back(cfg: &Config, started: Option<i64>) {
    if kept(cfg).is_none() {
        return;
    }
    let _lock = lock(cfg);
    let Some(mut s) = kept(cfg) else { return };
    let now = now();
    if s.back.is_none() {
        if started.is_some_and(|t| t <= s.refused()) {
            return;
        }
        s.back = Some(now);
        s.told = now;
        keep(cfg, &s);
        eprintln!("proj: Claude has usage again (out since {})", when(s.at));
        tell(cfg, &["close", NOTICE, CLOSED]);
        changed(cfg);
        return;
    }
    settle(cfg, s, now);
}

/// The owner's notice, with no run to ask (proj-watch's every look): while
/// it holds, its open again every RETELL ([`remind`]); once it has ended,
/// what's left settled ([`settle`]). When it next needs a look, while
/// there's anything left: one past its reset that no run has ended yet
/// waits for the next run that isn't refused, which closes it.
pub fn tend(cfg: &Config) -> Option<i64> {
    let s = kept(cfg)?;
    let now = now();
    if s.holds(now) {
        remind(cfg);
        return kept(cfg).map(|s| s.told + RETELL);
    }
    s.back?;
    let _lock = lock(cfg);
    let s = kept(cfg).filter(|s| s.back.is_some())?;
    settle(cfg, s, now);
    let s = kept(cfg)?;
    Some(if pending(cfg) {
        s.told + RETELL / 12
    } else {
        s.release() + cfg.stall_queued()
    })
}

/// What's left once it has ended, under the lock: the close sent again
/// until it got through, and the record gone a stall's time after the
/// release.
fn settle(cfg: &Config, mut s: Spent, now: i64) {
    let settled = !pending(cfg);
    if settled && now >= s.release() + cfg.stall_queued() {
        let _ = fs::remove_file(cfg.jobs.join(FILE));
    } else if !settled && now - s.told >= RETELL / 12 {
        s.told = now;
        keep(cfg, &s);
        tell(cfg, &["close", NOTICE, CLOSED]);
    }
}

/// What an earlier proj's own record of a usage limit is said as, once
/// it's taken into the hold.
pub const EARLIER: &str = "a usage limit an earlier proj or watchdog recorded";

/// The holds an earlier proj (#428's) kept apart from this one, each taken
/// into it: N.resets, the time a job's latest turn wrote that its usage limit
/// resets, any job's (an --owned one's included, whose starter would have
/// asked `proj resets`), and from a turn still running that proj, once it
/// ends. Each is read once and removed. The stall watchdog asks `proj usage`
/// every pass, which runs this first.
pub fn adopt(cfg: &Config) {
    let Ok(dirs) = fs::read_dir(&cfg.jobs) else {
        return;
    };
    for e in dirs.filter_map(|e| e.ok()) {
        let id = e.file_name().to_string_lossy().into_owned();
        if id.starts_with('.') {
            continue;
        }
        let job = Job::new(cfg, &id);
        let Some(n) = job.turns() else { continue };
        let f = job.turn(&n, "resets");
        let Ok(text) = fs::read_to_string(&f) else {
            continue;
        };
        if let Ok(t) = text.trim().parse::<i64>()
            && t > now()
        {
            record(cfg, &id, Some(t), EARLIER, now());
        }
        let _ = fs::remove_file(&f);
    }
}

const CLOSED: &str = "Claude has usage again, and proj's jobs are running.";

/// Whether HOST-notify may still have the notice open: a close that didn't
/// get through. Only its exit 1 says it's closed (or was never opened); one
/// that couldn't run, was killed or answered anything else may still have
/// it. Without HOST-notify, nothing is.
fn pending(cfg: &Config) -> bool {
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
