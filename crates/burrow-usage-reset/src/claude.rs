//! The account through Claude Code itself, as the machine's user: the meter
//! from its SDK's `get_usage` request, and the reset through its own
//! `/limit-reset weekly`, typed into a Claude Code of our own in a tmux, the
//! way the owner would at a terminal. It keys nothing but that command and
//! the confirmation of the reset, each only once the screen shows what it
//! answers; anything else ends the session, and ending Claude Code never uses
//! a reset. Its folder is trusted in Claude Code's config before a pass
//! starts (claude-trust, from the module), so a question about trusting it
//! ends the session too. Once its yes is pressed, the session lives until
//! Claude Code has its last word on it.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use std::{env, thread};

use serde_json::Value;

use crate::meter::Window;
use crate::pass::{Account, Reading, Reset};

/// How long Claude Code has to answer, or to show the next screen.
const TIMEOUT: Duration = Duration::from_secs(60);
/// How often the screen is looked at while waiting on it.
const POLL: Duration = Duration::from_millis(500);
/// How long the reset's dialog is left up before its yes is pressed: it
/// refuses keys that come too soon after it opens.
const PAUSE: Duration = Duration::from_secs(1);
/// A press its dialog shows no sign of after this was refused.
const REFUSED: Duration = Duration::from_secs(3);
/// Presses of its yes before the dialog is given up on.
const PRESSES: usize = 3;

pub struct ClaudeCode {
    /// The `claude` to run.
    pub claude: PathBuf,
    /// A tmux built not to move its panes out of this unit.
    pub tmux: PathBuf,
    /// The folder its Claude Code runs in, ours alone.
    pub dir: PathBuf,
}

impl Account for ClaudeCode {
    fn read(&mut self) -> Result<Reading, String> {
        read_usage(&self.claude, &self.dir)
    }

    fn reset(&mut self, keep: &mut dyn FnMut() -> Result<(), String>) -> Reset {
        let tmux = Tmux {
            bin: self.tmux.clone(),
        };
        let outcome = self.drive(&tmux, keep);
        tmux.kill();
        outcome
    }
}

/// The weekly meter from CLAUDE's `get_usage`, run in DIR, only if it can be
/// shown to be fresh (read_fresh).
pub fn read_usage(claude: &Path, dir: &Path) -> Result<Reading, String> {
    read_fresh(&mut copied_at, &mut crate::now, &mut || {
        ask_usage(claude, dir, TIMEOUT, None)
    })
    .map_err(Unread::said)
}

/// How long check_login may take in all: a copy of the meter waited out
/// (SNAPSHOT), another session's newer one waited out in turn, and a live
/// reading asked, inside HOST-health's timeout for it (modules/updates.nix
/// checkTimeouts.claude-login, 150s).
const LOGIN_BUDGET: Duration = Duration::from_secs(140);
/// The least of LOGIN_BUDGET a live reading is asked with: a copy whose wait
/// would leave less isn't waited out.
const ASK_LEAST: Duration = Duration::from_secs(10);

/// Why check_login didn't show the login working.
#[derive(Debug, PartialEq)]
pub enum Unshown {
    /// The login itself failed (login_shown): a new one is what fixes it.
    LoginFailed(String),
    /// Anything else, which says nothing against the login.
    Unknown(String),
}

/// Whether CLAUDE is logged in as the user it runs as, within LOGIN_BUDGET.
/// `get_usage` answers `success` to the request alone, and from a cached copy
/// or with null limits when the endpoint failed on an expired or revoked
/// login, so only a fresh answer from its endpoint shows it, and a copy's
/// recency shows nothing about the login now (login_within). A reading that
/// fails on the endpoint's rate limit alone doesn't fail the login
/// (login_shown): Claude Code's debug log says which it was.
pub fn check_login(claude: &Path, dir: &Path) -> Result<(), Unshown> {
    let start = Instant::now();
    // Its debug log, in a private folder of its own, removed on drop.
    let logs = tempfile::Builder::new()
        .prefix("debug-")
        .tempdir_in(dir)
        .map_err(|e| Unshown::Unknown(format!("a folder for Claude Code's debug log: {e}")))?;
    let log = logs.path().join("claude.log");
    login_within(
        LOGIN_BUDGET,
        &mut copied_at,
        &mut crate::now,
        &mut || start.elapsed(),
        &mut thread::sleep,
        &mut |left| {
            // Each asking's log on its own: an earlier one's says nothing.
            let _ = std::fs::remove_file(&log);
            ask_usage(claude, dir, left, Some(&log))
        },
        &mut || String::from_utf8_lossy(&std::fs::read(&log).unwrap_or_default()).into_owned(),
    )
}

/// check_login's steps, with the copy's time (COPIED), the clock, the time
/// spent (ELAPSED), the waiting (SLEEP) and the asking (ASK, given the time
/// left) and the debug LOG of the last asking handed in. A copy under SNAPSHOT old would be what answers, so it is
/// waited out first; then a live reading of the meter (read_fresh's) is
/// asked. The user's other Claude Code sessions fetch the meter too, and may
/// write a new copy during the wait or the asking: that says nothing about
/// the login, so the new copy is waited out in turn, while BUDGET leaves
/// ASK_LEAST to ask with. A copy dated ahead of this clock proves nothing and
/// is not waited out: the reading refuses it. A reading that failed is
/// judged by login_shown.
fn login_within(
    budget: Duration,
    copied: &mut dyn FnMut() -> Result<Option<f64>, String>,
    clock: &mut dyn FnMut() -> f64,
    elapsed: &mut dyn FnMut() -> Duration,
    sleep: &mut dyn FnMut(Duration),
    ask: &mut dyn FnMut(Duration) -> Result<Value, String>,
    log: &mut dyn FnMut() -> String,
) -> Result<(), Unshown> {
    loop {
        let seen = copied().map_err(Unshown::Unknown)?;
        if let Some(at) = seen {
            let age = clock() - at;
            if (0.0..SNAPSHOT).contains(&age) {
                let wait = Duration::from_secs_f64(SNAPSHOT - age + 1.0);
                if elapsed() + wait + ASK_LEAST > budget {
                    return Err(Unshown::Unknown(format!(
                        "Claude Code's cached meter was fetched {age:.0}s ago, by another of this user's sessions, and waiting it out leaves too little of {}s to ask its endpoint: the login wasn't checked",
                        budget.as_secs()
                    )));
                }
                sleep(wait);
                continue;
            }
        }
        let left = budget.saturating_sub(elapsed());
        if left < ASK_LEAST {
            return Err(Unshown::Unknown(format!(
                "another of this user's sessions kept refreshing Claude Code's cached meter, leaving too little of {}s to ask its endpoint: the login wasn't checked",
                budget.as_secs()
            )));
        }
        let mut answer = Value::Null;
        let failed = match read_fresh(copied, clock, &mut || {
            answer = ask(left)?;
            Ok(answer.clone())
        }) {
            Ok(_) => return Ok(()),
            Err(e) => e,
        };
        let copy = matches!(failed, Unread::Copy(_));
        match login_shown(Err(failed.said()), &answer, &log()) {
            Ok(()) => return Ok(()),
            // Another session's copy, written since it was looked at, may be
            // what answered: wait that one out.
            Err(_) if copy && copied().map_err(Unshown::Unknown)? != seen => continue,
            Err(e) => return Err(e),
        }
    }
}

/// Whether the login is shown working, given a live READING of the meter,
/// the ANSWER it was read from and Claude Code's debug LOG of the asking. A
/// reading is the proof; a failed one still leaves the login working when
/// the usage endpoint answered nothing but 429 (usage_statuses) and the
/// answer has plan limits. That endpoint is shared by every Claude Code
/// logged in as this user, and a 429 is remembered for the bearer for
/// minutes, so it often answers from its copy on a rate limit; an expired or
/// revoked login gets a 401 or no plan limits instead. With no 429 in the
/// log, or anything else beside it, the reading's error stands, with the
/// endpoint's failure from the log.
///
/// The login itself failed only on an answer with no plan limits (logged
/// out, or its tokens cleared once their refresh was refused) or when the
/// endpoint refused it and then so was its refresh (login_refused). A 401
/// alone doesn't show that: an access token that ran out while its refresh
/// couldn't get through gets one too, and the next refresh mends it.
fn login_shown(reading: Result<(), String>, answer: &Value, log: &str) -> Result<(), Unshown> {
    let Err(failed) = reading else {
        return Ok(());
    };
    match answer["rate_limits_available"].as_bool() {
        Some(false) => return Err(Unshown::LoginFailed(failed)),
        // No answer, so the log says nothing about this asking.
        None => return Err(Unshown::Unknown(failed)),
        Some(true) => {}
    }
    let statuses = usage_statuses(log);
    if !statuses.is_empty() && statuses.iter().all(|status| *status == 429) {
        return Ok(());
    }
    let failed = match endpoint_failure(log) {
        Some(why) => format!("{failed} (its usage endpoint: {why})"),
        None => failed,
    };
    Err(if login_refused(log) {
        Unshown::LoginFailed(failed)
    } else {
        Unshown::Unknown(failed)
    })
}

/// What begins Claude Code's debug line on a reading it couldn't load.
const NOT_LOADED: &str = "Failed to load usage data:";
/// And on a refresh of the login that failed, refused or not got through.
const NOT_REFRESHED: &str = "OAuth refresh failed (expected):";
/// What both say after that when an HTTP status was the failure.
const STATUS: &str = "Request failed with status code ";

/// Whether the debug LOG shows the login refused for good: the reading's
/// last word a 401 or 403 (a revoked token's), and its refresh refused with
/// a 400 or 401, on which 2.1.330 drops the tokens. A refresh that met a
/// 5xx, no network or another process's lock is none of these.
fn login_refused(log: &str) -> bool {
    let status = |said: &str| said.trim().strip_prefix(STATUS)?.parse::<u16>().ok();
    let last = log.lines().rev().find_map(|l| l.split_once(NOT_LOADED));
    matches!(last.and_then(|(_, said)| status(said)), Some(401 | 403))
        && log
            .lines()
            .filter_map(|l| l.split_once(NOT_REFRESHED))
            .any(|(_, said)| matches!(status(said), Some(400 | 401)))
}

/// How the debug LOG says the usage endpoint last failed: the message of its
/// NOT_LOADED line, or the status alone from the error 2.1.330 logs in its
/// place on a 5xx, whose response body stays out of the journal.
fn endpoint_failure(log: &str) -> Option<String> {
    log.lines().rev().find_map(|line| {
        if let Some((_, said)) = line.split_once(NOT_LOADED) {
            return Some(said.trim().to_owned());
        }
        let (_, said) = line.split_once("/api/oauth/usage,status=")?;
        let status: String = said.chars().take_while(char::is_ascii_digit).collect();
        Some(format!("{STATUS}{status}"))
    })
}

/// The HTTP statuses Claude Code's debug LOG shows its usage endpoint
/// answering: the three-digit numbers on its lines about that endpoint,
/// which 2.1.330 writes as `fetchUtilization: 429 remembered for this
/// bearer; not asking again for 300s`, `fetchUtilization: 200 after 1
/// attempt(s)` and `Failed to load usage data: Request failed with status
/// code 429` (checked against remi's). Should a later Claude Code word them
/// otherwise, none are found and a rate limit fails the login again.
fn usage_statuses(log: &str) -> Vec<u16> {
    const ABOUT: [&str; 2] = ["fetchUtilization:", NOT_LOADED];
    log.lines()
        .filter_map(|line| {
            ABOUT
                .iter()
                .find_map(|about| line.split_once(about).map(|(_, said)| said))
        })
        .flat_map(|said| said.split(|c: char| !c.is_ascii_alphanumeric()))
        .filter(|word| word.len() == 3 && word.bytes().all(|b| b.is_ascii_digit()))
        .filter_map(|word| word.parse().ok())
        .filter(|status| (100..600).contains(status))
        .collect()
}

/// Claude Code's answer to `get_usage`, without the scan of local
/// transcripts the meter has no use for. It makes no model call. With a
/// LOG, Claude Code writes its debug log there.
fn ask_usage(
    claude: &Path,
    dir: &Path,
    timeout: Duration,
    log: Option<&Path>,
) -> Result<Value, String> {
    {
        let mut command = Command::new(claude);
        command
            .args([
                "-p",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--verbose",
            ])
            .args(["--strict-mcp-config", "--no-session-persistence"]);
        if let Some(log) = log {
            command.arg("--debug-file").arg(log);
        }
        let mut child = command
            .current_dir(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("{}: {e}", claude.display()))?;
        let request = r#"{"type":"control_request","request_id":"usage","request":{"subtype":"get_usage","skip_behaviors":true}}"#;
        let sent = child
            .stdin
            .take()
            .map(|mut stdin| writeln!(stdin, "{request}"))
            .transpose()
            .map_err(|e| format!("claude's stdin: {e}"));
        let (tx, rx) = mpsc::channel();
        if let Some(stdout) = child.stdout.take() {
            thread::spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if let Ok(v) = serde_json::from_str::<Value>(&line)
                        && v["type"] == "control_response"
                        && v["response"]["request_id"] == "usage"
                    {
                        let _ = tx.send(v["response"].clone());
                        return;
                    }
                }
            });
        }
        let answer = sent.and_then(|_| {
            rx.recv_timeout(timeout).map_err(|e| match e {
                mpsc::RecvTimeoutError::Timeout => format!(
                    "Claude Code didn't answer get_usage in {}s",
                    timeout.as_secs()
                ),
                mpsc::RecvTimeoutError::Disconnected => {
                    "Claude Code exited before it answered get_usage".to_owned()
                }
            })
        });
        let _ = child.kill();
        let _ = child.wait();
        let answer = answer?;
        if answer["subtype"] != "success" {
            return Err(format!(
                "get_usage: {}",
                answer["error"].as_str().unwrap_or("failed")
            ));
        }
        Ok(answer["response"].clone())
    }
}

impl ClaudeCode {
    fn drive(&self, tmux: &Tmux, keep: &mut dyn FnMut() -> Result<(), String>) -> Reset {
        if let Err(e) = tmux.start(&self.dir, &self.claude) {
            return Reset::NotPressed(e);
        }
        // Up to its input box.
        match tmux.wait(|screen| {
            if prompt_ready(screen) {
                Some(true)
            } else {
                asks_trust(screen).then_some(false)
            }
        }) {
            Ok(true) => {}
            Ok(false) => {
                return Reset::NotPressed(
                    "Claude Code asked whether to trust its folder, which claude-trust marks trusted before each pass: the unit's journal says why it didn't".to_owned(),
                );
            }
            Err(e) => return Reset::NotPressed(format!("Claude Code didn't come up: {e}")),
        }

        if let Err(e) = tmux.literal(COMMAND).and_then(|()| tmux.keys(&["Enter"])) {
            return Reset::NotPressed(e);
        }
        // Its answer: a confirmation, its cursor brought to its yes, or a
        // line saying why there's nothing to confirm.
        let mut moves = 0;
        loop {
            let seen = tmux.wait(|screen| match read_reset(screen) {
                Screen::Busy => None,
                other => Some(other),
            });
            match seen {
                Err(e) => return Reset::NotPressed(format!("no answer to {COMMAND}: {e}")),
                Ok(Screen::Busy) => unreachable!(),
                Ok(Screen::Said(r)) => return r,
                Ok(Screen::Confirm(Cursor::OnYes)) => break,
                Ok(Screen::Confirm(cursor)) if moves < 3 => {
                    moves += 1;
                    let key = if cursor == Cursor::Below {
                        "Up"
                    } else {
                        "Down"
                    };
                    if let Err(e) = tmux.keys(&[key]) {
                        return Reset::NotPressed(e);
                    }
                    thread::sleep(POLL);
                }
                Ok(Screen::Confirm(_)) => {
                    return Reset::NotPressed(
                        "couldn't put the cursor on the reset's yes".to_owned(),
                    );
                }
            }
        }
        if let Err(e) = keep() {
            return Reset::NotPressed(format!(
                "couldn't keep that this week's reset is being used, so it wasn't: {e}"
            ));
        }
        self.confirm(tmux)
    }

    /// Its yes, pressed until its dialog takes it, then Claude Code's last
    /// word on it. Everything from the first press on may have used the
    /// reset, so it's Pressed whatever it comes to, even a dialog that
    /// looks to have taken none: the meter says.
    fn confirm(&self, tmux: &Tmux) -> Reset {
        for _ in 0..PRESSES {
            thread::sleep(PAUSE);
            if let Err(e) = tmux.keys(&["Enter"]) {
                return Reset::Pressed(format!("pressing its yes failed: {e}"));
            }
            let pressed = Instant::now();
            let mut last: Option<String> = None;
            let seen = tmux.wait(|screen| {
                if still_asking(screen) && pressed.elapsed() >= REFUSED {
                    return Some(None);
                }
                // Its last word, the same on two looks in a row: a frame
                // half drawn is no answer.
                let word = finished(screen);
                if word.is_some() && word == last {
                    return Some(word);
                }
                last = word;
                None
            });
            match seen {
                Ok(Some(said)) => return Reset::Pressed(said),
                Ok(None) => continue,
                Err(e) => return Reset::Pressed(format!("no last word on it: {e}")),
            }
        }
        Reset::Pressed(format!(
            "its dialog showed none of {PRESSES} presses of its yes taken"
        ))
    }
}

/// How old Claude Code's persisted copy of the meter can be for `get_usage`
/// to answer from it without asking its endpoint at all, in seconds.
const SNAPSHOT: f64 = 60.0;

/// When Claude Code's persisted copy of the meter was fetched, in seconds
/// since the epoch: its global config's `cachedUsageUtilization`, for this
/// user.
fn copied_at() -> Result<Option<f64>, String> {
    let dir = env::var_os("CLAUDE_CONFIG_DIR")
        .or_else(|| env::var_os("HOME"))
        .ok_or("neither CLAUDE_CONFIG_DIR nor HOME is set")?;
    let file = PathBuf::from(dir).join(".claude.json");
    match std::fs::read(&file) {
        Ok(config) => copied_in(&config).map_err(|e| format!("{}: {e}", file.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", file.display())),
    }
}

/// The copy's time in Claude Code's config. None only if the config shows it
/// has no copy: one it can't make sense of may still be what answers.
fn copied_in(config: &[u8]) -> Result<Option<f64>, String> {
    let config: Value = serde_json::from_slice(config).map_err(|e| e.to_string())?;
    match config.get("cachedUsageUtilization") {
        None | Some(Value::Null) => Ok(None),
        Some(copy) => copy["fetchedAtMs"]
            .as_f64()
            .map(|ms| Some(ms / 1000.0))
            .ok_or_else(|| "its cached meter has no fetchedAtMs".to_owned()),
    }
}

/// Why `get_usage` gave no reading.
#[derive(Debug, PartialEq)]
enum Unread {
    /// Claude Code's copy of the meter may be what answered: it changed while
    /// it was asked, or was under SNAPSHOT old when it was.
    Copy(String),
    /// Anything else.
    Not(String),
}

impl Unread {
    fn said(self) -> String {
        match self {
            Unread::Copy(said) | Unread::Not(said) => said,
        }
    }
}

/// The meter from one `get_usage` (ASK), only if it can be shown to be fresh.
/// The copy is looked at first, then the clock, then it's asked: Claude Code
/// picks its answer after all of them, so a copy already past SNAPSHOT by this
/// clock can't be what answers, however long the asking takes. One dated ahead
/// of this clock could be, once the clock catches up, so it counts as under
/// SNAPSHOT. A copy that
/// changed while it was asked may be what answered, and there's no telling, so
/// that's no reading either. (A read of its own doesn't change it: it's killed
/// on its answer, before it writes the copy.)
fn read_fresh(
    copied: &mut dyn FnMut() -> Result<Option<f64>, String>,
    clock: &mut dyn FnMut() -> f64,
    ask: &mut dyn FnMut() -> Result<Value, String>,
) -> Result<Reading, Unread> {
    let before = copied().map_err(Unread::Not)?;
    let asked = clock();
    let answer = ask().map_err(Unread::Not)?;
    let after = copied().map_err(Unread::Not)?;
    if after != before {
        return Err(Unread::Copy(
            "Claude Code's cached meter changed while get_usage was answering, so its answer may be that copy"
                .to_owned(),
        ));
    }
    usage(&answer, before, asked).map_err(|e| match before {
        Some(at) if asked - at < SNAPSHOT => Unread::Copy(e),
        _ => Unread::Not(e),
    })
}

/// The weekly meter in a `get_usage` answer, asked at NOW with Claude Code's
/// persisted copy of it fetched at COPIED. An answer that didn't come from
/// its endpoint just now is an error, so a pass treats it as a meter it
/// couldn't read: it presses nothing, settles nothing and books nothing on
/// it, and tries again soon.
///
/// Nothing in an answer says it's cached, but two things tell (Claude Code
/// 2.1.316, checked against a live one):
/// - Its endpoint refused or failed: the answer is the persisted copy, up to
///   an hour old, or the last rate-limit headers, and has no
///   `rate_limits.limits`, which its endpoint's answers have. Should a later
///   Claude Code or endpoint drop that field, every read here fails and the
///   owner hears the meter can't be read.
/// - A copy under SNAPSHOT old: its endpoint isn't asked and the answer is
///   the copy, `limits` and all. It's a minute old at most, but that's the
///   whole of the time between a reading and a reset's yes: read after it, it
///   would say the reset didn't take.
pub fn usage(answer: &Value, copied: Option<f64>, now: f64) -> Result<Reading, String> {
    if answer["rate_limits_available"] != true {
        return Err(format!(
            "Claude Code has no plan limits to show (subscription {}): is it logged in to a claude.ai plan here?",
            answer["subscription_type"]
        ));
    }
    let week = &answer["rate_limits"]["seven_day"];
    let used = week["utilization"]
        .as_f64()
        .ok_or("get_usage has no weekly utilization")?;
    let resets = week["resets_at"]
        .as_str()
        .ok_or("get_usage has no weekly resets_at")?;
    let ends = resets
        .parse::<jiff::Timestamp>()
        .map_err(|e| format!("get_usage's weekly resets_at {resets}: {e}"))?;
    let age = copied.map(|at| now - at);
    if !answer["rate_limits"]["limits"].is_array() {
        return Err(match age {
            Some(age) => format!(
                "get_usage answered {used}% from Claude Code's cached meter, its usage endpoint not answering; the copy was fetched {age:.0}s ago"
            ),
            None => format!(
                "get_usage answered {used}% from Claude Code's cached meter, its usage endpoint not answering"
            ),
        });
    }
    if let Some(age) = age.filter(|age| *age < SNAPSHOT) {
        return Err(format!(
            "get_usage answered {used}% from a snapshot {age:.0}s old, its usage endpoint not asked"
        ));
    }
    // To the minute: the same week's end may come back with a different
    // fraction of a second.
    Ok(Reading {
        used,
        week: resets.chars().take(16).collect(),
        ends: ends.as_second() as f64,
        session: session(&answer["rate_limits"]["five_hour"])?,
    })
}

/// The five-hour window in a `get_usage` answer: null while no session
/// runs, and an error when it's there but can't be read.
fn session(window: &Value) -> Result<Option<Window>, String> {
    if window.is_null() {
        return Ok(None);
    }
    let used = window["utilization"]
        .as_f64()
        .ok_or("get_usage's five_hour has no utilization")?;
    let resets = window["resets_at"]
        .as_str()
        .ok_or("get_usage's five_hour has no resets_at")?;
    let at = resets
        .parse::<jiff::Timestamp>()
        .map_err(|e| format!("get_usage's five_hour resets_at {resets}: {e}"))?;
    Ok(Some(Window {
        used,
        resets_at: at.as_second() as f64,
    }))
}

/// What it types.
const COMMAND: &str = "/limit-reset weekly";
/// The yes of its question about trusting the folder it was started in.
const TRUST: &str = "Yes, I trust this folder";
/// The confirmation's yes, as Claude Code words it unless its server says
/// otherwise: "Yes, use my weekly reset".
const YES: &str = "Yes, use my";
/// What its dialog shows once its yes is taken, while the reset goes
/// through.
const WORKING: &str = "Resetting your limits";

/// What the screen shows after COMMAND.
#[derive(Debug, PartialEq)]
enum Screen {
    /// Still drawing, or still checking.
    Busy,
    /// A confirmation of the reset, and where its cursor is.
    Confirm(Cursor),
    /// A line that settles it without confirming anything.
    Said(Reset),
}

/// Where a confirmation's cursor is, against its yes.
#[derive(Debug, PartialEq)]
enum Cursor {
    OnYes,
    Above,
    Below,
}

fn read_reset(screen: &str) -> Screen {
    // Only its confirmation has its yes, so it counts wherever it's drawn.
    let lines: Vec<&str> = screen.lines().collect();
    if let Some(yes) = lines.iter().position(|l| l.contains(YES)) {
        // The choices' cursor, not the input box's (a `❯` under a rule) nor
        // COMMAND's echo.
        let cursor = (0..lines.len()).find(|&i| {
            lines[i].trim_start().starts_with('❯')
                && !(i > 0 && is_rule(lines[i - 1]))
                && !lines[i].trim_end().ends_with(COMMAND)
        });
        return Screen::Confirm(match cursor {
            Some(c) if c == yes => Cursor::OnYes,
            Some(c) if c > yes => Cursor::Below,
            // No cursor drawn yet reads as above: Down puts it on the first
            // choice, and the next look sees where.
            _ => Cursor::Above,
        });
    }
    let Some(lines) = answer(screen) else {
        return Screen::Busy;
    };
    let text = lines.join("\n");
    let has = |s: &str| text.contains(s);
    let said = said(screen);
    // Claude Code's own lines (Claude Code 2.1.314, limit-reset): the ones
    // that settle it without using anything.
    if has("only works at a usage limit")
        || has("saved for when you reach a usage limit")
        || has("at a limit to use it")
        || has("once you reach a limit")
        || has("not at a usage limit")
    {
        Screen::Said(Reset::AtLimitOnly(said))
    } else if has("No reset to use") || has("No weekly reset to use") {
        Screen::Said(Reset::NoneBanked(said))
    } else if has("nothing was used")
        || has("can't be used")
        || has("Couldn't")
        || has(" kept")
        || has("None of your resets refill")
        // A Claude Code without the command: the owner hears, after its
        // tries, rather than it passing for an empty bank.
        || has("Unknown command")
        || has("Unknown slash command")
    {
        Screen::Said(Reset::NotPressed(said))
    } else {
        Screen::Busy
    }
}

/// What Claude Code answered COMMAND with: the lines under its echo in the
/// transcript, down to its input box. None until it has echoed it: before
/// then the screen is the notices it started with, the command still in its
/// box, or a frame drawn before the answer.
fn answer(screen: &str) -> Option<Vec<&str>> {
    let lines: Vec<&str> = screen.lines().collect();
    let echo = (0..lines.len())
        .rev()
        .find(|&i| lines[i].trim_end().ends_with(COMMAND) && !(i > 0 && is_rule(lines[i - 1])))?;
    let end = (echo + 1..lines.len())
        .find(|&i| {
            is_rule(lines[i])
                && lines
                    .get(i + 1)
                    .is_some_and(|l| l.trim_start().starts_with('❯'))
        })
        .unwrap_or(lines.len());
    Some(lines[echo + 1..end].to_vec())
}

/// Its answer's words, as one line for a notice.
fn said(screen: &str) -> String {
    let words: Vec<&str> = answer(screen)
        .unwrap_or_default()
        .into_iter()
        .map(|l| l.trim().trim_start_matches(['⎿', '❯', '●', ' ']))
        .filter(|l| !l.is_empty() && !l.chars().all(|c| "─▔━".contains(c)))
        .collect();
    words.join(" / ").chars().take(300).collect()
}

/// Its dialog is up and still asking, its yes not taken.
fn still_asking(screen: &str) -> bool {
    screen.contains(YES) && !screen.contains(WORKING) && !prompt_ready(screen)
}

/// After its yes: what it answered, once its dialog is gone and it's back at
/// its input box.
fn finished(screen: &str) -> Option<String> {
    if screen.contains(YES) || screen.contains(WORKING) || !prompt_ready(screen) {
        return None;
    }
    Some(said(screen)).filter(|s| !s.is_empty())
}

/// Its input box is up: a `❯` line right under a rule.
fn prompt_ready(screen: &str) -> bool {
    let lines: Vec<&str> = screen.lines().collect();
    lines
        .windows(2)
        .any(|w| is_rule(w[0]) && w[1].trim_start().starts_with('❯'))
}

/// A line drawn across the screen, as above and below its input box.
fn is_rule(line: &str) -> bool {
    let line = line.trim();
    !line.is_empty() && line.chars().all(|c| c == '─')
}

/// It's asking whether to trust the folder it was started in, wherever its
/// cursor is.
fn asks_trust(screen: &str) -> bool {
    !prompt_ready(screen) && screen.contains(TRUST)
}

/// A tmux server of our own, for one Claude Code.
struct Tmux {
    bin: PathBuf,
}

const SOCKET: &str = "burrow-usage-reset";

impl Tmux {
    fn run(&self, args: &[&str]) -> Result<String, String> {
        let out = Command::new(&self.bin)
            .args(["-L", SOCKET])
            .args(args)
            .output()
            .map_err(|e| format!("tmux: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "tmux {}: {}",
                args[0],
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn start(&self, dir: &std::path::Path, claude: &std::path::Path) -> Result<(), String> {
        // One left over from a pass that died is no use to this one.
        self.kill();
        let dir = dir.to_string_lossy();
        let claude = claude.to_string_lossy();
        self.run(&[
            "new-session",
            "-d",
            "-s",
            "reset",
            "-x",
            "200",
            "-y",
            "50",
            "-c",
            &dir,
            &claude,
            "--strict-mcp-config",
        ])
        .map(drop)
    }

    fn screen(&self) -> Result<String, String> {
        self.run(&["capture-pane", "-p", "-t", "reset"])
    }

    fn keys(&self, keys: &[&str]) -> Result<(), String> {
        let mut args = vec!["send-keys", "-t", "reset"];
        args.extend_from_slice(keys);
        self.run(&args).map(drop)
    }

    fn literal(&self, text: &str) -> Result<(), String> {
        self.run(&["send-keys", "-t", "reset", "-l", text])
            .map(drop)
    }

    /// Look at the screen until SEEN finds what it waits for; the last
    /// screen's lines, if it never does.
    fn wait<T>(&self, mut seen: impl FnMut(&str) -> Option<T>) -> Result<T, String> {
        let start = Instant::now();
        let mut last = String::new();
        while start.elapsed() < TIMEOUT {
            last = self.screen()?;
            if let Some(t) = seen(&last) {
                return Ok(t);
            }
            thread::sleep(POLL);
        }
        let tail: Vec<&str> = last
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        Err(format!(
            "timed out; the screen ends: {}",
            tail[tail.len().saturating_sub(4)..].join(" / ")
        ))
    }

    fn kill(&self) {
        let _ = self.run(&["kill-server"]);
    }
}

#[cfg(test)]
mod tests;
