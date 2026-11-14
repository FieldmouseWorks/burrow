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
        ask_usage(claude, dir, TIMEOUT)
    })
}

/// How long check_login may take in all: a copy of the meter is waited out
/// (SNAPSHOT at most) and then one live reading asked, inside HOST-health's
/// check timeout (modules/updates.nix checkTimeout, 90s).
const LOGIN_BUDGET: Duration = Duration::from_secs(80);

/// Whether CLAUDE is logged in as the user it runs as, within LOGIN_BUDGET.
/// `get_usage` answers `success` to the request alone, and from a cached copy
/// or with null limits when the endpoint failed on an expired or revoked
/// login, so only a fresh answer from its endpoint shows it, and a copy's
/// recency shows nothing about the login now. A copy under SNAPSHOT old would
/// be what answers, so it is waited out first; then a live reading of the
/// meter (read_usage's) is asked, whatever the copy's age. A copy dated ahead
/// of this clock proves nothing and is not waited out: the reading refuses it.
pub fn check_login(claude: &Path, dir: &Path) -> Result<(), String> {
    let start = Instant::now();
    if let Some(at) = copied_at()? {
        let age = crate::now() - at;
        if (0.0..SNAPSHOT).contains(&age) {
            thread::sleep(Duration::from_secs_f64(SNAPSHOT - age + 1.0));
        }
    }
    let left = LOGIN_BUDGET.saturating_sub(start.elapsed());
    read_fresh(&mut copied_at, &mut crate::now, &mut || {
        ask_usage(claude, dir, left)
    })
    .map(|_| ())
}

/// Claude Code's answer to `get_usage`, without the scan of local
/// transcripts the meter has no use for. It makes no model call.
fn ask_usage(claude: &Path, dir: &Path, timeout: Duration) -> Result<Value, String> {
    {
        let mut child = Command::new(claude)
            .args([
                "-p",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--verbose",
            ])
            .args(["--strict-mcp-config", "--no-session-persistence"])
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
            rx.recv_timeout(timeout)
                .map_err(|_| "Claude Code didn't answer get_usage".to_owned())
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
) -> Result<Reading, String> {
    let before = copied()?;
    let asked = clock();
    let answer = ask()?;
    let after = copied()?;
    if after != before {
        return Err(
            "Claude Code's cached meter changed while get_usage was answering, so its answer may be that copy"
                .to_owned(),
        );
    }
    usage(&answer, before, asked)
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
