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
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use std::{env, thread};

use serde_json::Value;

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
        read_fresh(&mut copied_at, &mut crate::now, &mut || self.ask_usage())
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

impl ClaudeCode {
    /// Claude Code's answer to `get_usage`, without the scan of local
    /// transcripts the meter has no use for. It makes no model call.
    fn ask_usage(&self) -> Result<Value, String> {
        let mut child = Command::new(&self.claude)
            .args([
                "-p",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--verbose",
            ])
            .args(["--strict-mcp-config", "--no-session-persistence"])
            .current_dir(&self.dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("{}: {e}", self.claude.display()))?;
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
            rx.recv_timeout(TIMEOUT)
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
    })
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
mod tests {
    use super::*;
    use crate::pass::{Notice, Outcome, RETRY, State, pass};
    use serde_json::json;

    /// Its screen once it's up, as remi's Claude Code 2.1.314 drew it
    /// (remi#208), its notices above the input box.
    const UP: &str = "
 ▐▛███▛█   Claude Code v2.1.314
▝▜██████▀  Opus 5.5 · Claude Max
 ▝▝   ▝▝   /var/lib/burrow/usage-reset/claude

  Get to finished work sooner with Opus 5.5. Switch anytime with /model.
  /limit-reset to use your weekly reset · 1 left · use by Nov 6
  Weekly reset kept · run /limit-reset at a limit to use it · use by Nov 6

                                You've used 71% of your weekly limit · resets Nov 4, 12pm (America/Los_Angeles)
────────────────────────────────────────────────────────────────────────────────
❯ Try \"create a util logging.py that...\"
────────────────────────────────────────────────────────────────────────────────
  ⏵⏵ bypass permissions on (shift+tab to cycle) · ← for agents
";

    /// The screen once COMMAND is in and answered with LINES.
    fn answered(lines: &str) -> String {
        let top = UP.split("────").next().unwrap();
        format!("{top}❯ /limit-reset weekly\n{lines}\n────────\n❯ \n────────\n")
    }

    #[test]
    fn its_input_box() {
        assert!(prompt_ready(UP));
        assert!(!asks_trust(UP));
        // Claude Code 2.1.314's, as remi's tmux showed it: its no first, and
        // the cursor on it.
        let trust = "
 Accessing workspace:

 /var/lib/remi-update/usage-reset/claude

 Quick safety check: Is this a project you created or one you trust? (Like your own code, a well-known open source project, or work from your team). If not, take a moment to review what's in this folder first.

 Claude Code'll be able to read, edit, and execute files here.

 Security guide

 ❯ No, exit
   Yes, I trust this folder

 Enter to confirm · Esc to cancel
";
        assert!(asks_trust(trust) && !prompt_ready(trust));
        assert!(asks_trust(
            &trust
                .replace("❯ No, exit", "  No, exit")
                .replace("  Yes, I trust", "❯ Yes, I trust")
        ));
    }

    #[test]
    fn only_its_answer_counts() {
        // The notices it started with (one says "at a limit to use it")
        // settle nothing: not before COMMAND is typed, nor with it still in
        // the box, nor in a frame drawn after Enter and before the echo.
        assert_eq!(read_reset(UP), Screen::Busy);
        let typed = UP.replace(
            "❯ Try \"create a util logging.py that...\"",
            "❯ /limit-reset weekly",
        );
        assert_eq!(read_reset(&typed), Screen::Busy);
        assert_eq!(
            read_reset(&format!("{UP}❯ /limit-reset weekly\n")),
            Screen::Busy
        );
        assert_eq!(
            read_reset(&answered("  ⎿  Checking for a reset…")),
            Screen::Busy
        );
    }

    #[test]
    fn its_confirmation_and_where_the_cursor_is() {
        let dialog = |yes: &str, no: &str| {
            answered(&format!(
                " Use a weekly reset?\n Refills your weekly limit now · your weekly reset day stays Wednesday\n 0 weekly resets left · use by Nov 6\n\n {yes}1. Yes, use my weekly reset\n {no}2. No, keep it\n"
            ))
        };
        assert_eq!(
            read_reset(&dialog("❯ ", "  ")),
            Screen::Confirm(Cursor::OnYes)
        );
        assert_eq!(
            read_reset(&dialog("  ", "❯ ")),
            Screen::Confirm(Cursor::Below)
        );
        assert_eq!(
            read_reset(&dialog("  ", "  ")),
            Screen::Confirm(Cursor::Above)
        );
        // Drawn over the transcript, its echo gone.
        assert_eq!(
            read_reset(" Use a weekly reset?\n\n ❯ Yes, use my weekly reset\n   No, keep it\n"),
            Screen::Confirm(Cursor::OnYes)
        );
    }

    #[test]
    fn its_lines_that_use_nothing() {
        let said = |line: &str| read_reset(&answered(&format!("  ⎿  {line}")));
        assert_eq!(
            said("No reset to use"),
            Screen::Said(Reset::NoneBanked("No reset to use".to_owned()))
        );
        assert!(matches!(
            said("No weekly reset to use · this limit resets Nov 4"),
            Screen::Said(Reset::NoneBanked(_))
        ));
        assert!(matches!(
            said(
                "Your reset only works at a usage limit, and you're not at one now · nothing was used"
            ),
            Screen::Said(Reset::AtLimitOnly(_))
        ));
        assert!(matches!(
            said(
                "You have a reset saved for when you reach a usage limit · run /limit-reset when you do"
            ),
            Screen::Said(Reset::AtLimitOnly(_))
        ));
        assert!(matches!(
            said("Couldn't reset your limits · nothing was used · try again in a moment"),
            Screen::Said(Reset::NotPressed(_))
        ));
        assert!(matches!(
            said(
                "Your weekly reset can't be used right now · usable once you reach a limit it refills · you can use nothing here"
            ),
            Screen::Said(Reset::AtLimitOnly(_))
        ));
        assert!(matches!(
            said("None of your resets refill your weekly limit"),
            Screen::Said(Reset::NotPressed(_))
        ));
        // A Claude Code without it isn't an empty bank.
        assert!(matches!(
            said("Unknown command: /limit-reset"),
            Screen::Said(Reset::NotPressed(_))
        ));
        assert!(matches!(
            said("Unknown slash command: limit-reset"),
            Screen::Said(Reset::NotPressed(_))
        ));
    }

    #[test]
    fn its_last_word_after_the_yes() {
        let dialog = " Use a weekly reset?\n Refills your weekly limit now\n\n ❯ Yes, use my weekly reset\n   No, keep it\n";
        let top = UP.split("────").next().unwrap();
        let up = format!("{top}❯ /limit-reset weekly\n{dialog}");
        // Still asking: the press wasn't taken.
        assert!(still_asking(&up) && finished(&up).is_none());
        // Taken, and going through.
        let working = format!(
            "{top}❯ /limit-reset weekly\n Use a weekly reset?\n Resetting your limits… this usually takes a few seconds\n"
        );
        assert!(!still_asking(&working) && finished(&working).is_none());
        // The same with its input box drawn under it.
        let boxed = format!("{working}────────\n❯ \n────────\n");
        assert!(finished(&boxed).is_none());
        // Back at its input box, with nothing under the echo yet.
        assert!(finished(&answered("")).is_none());
        let done = answered(
            "  ⎿  Weekly limit refilled · your weekly reset day stays Wednesday · 0 weekly resets left",
        );
        assert!(!still_asking(&done));
        assert_eq!(
            finished(&done).as_deref(),
            Some(
                "Weekly limit refilled · your weekly reset day stays Wednesday · 0 weekly resets left"
            )
        );
        let unsure =
            answered("  ⎿  Couldn't confirm the reset went through · check /usage in a moment");
        assert!(finished(&unsure).is_some());
    }

    /// An answer as Claude Code's endpoint gave it, 2.1.316's fields.
    fn fetched(used: f64) -> Value {
        json!({
            "session": {"total_cost_usd": 0},
            "subscription_type": "max",
            "rate_limits_available": true,
            "rate_limits": {
                "five_hour": {"utilization": 35.0, "resets_at": "2026-10-30T06:40:00.412Z"},
                "seven_day": {"utilization": used, "resets_at": "2026-11-04T19:00:00.412Z"},
                "limits": [{"kind": "weekly_all", "group": "weekly", "percent": used}]
            },
            "behaviors": null
        })
    }

    /// The same, answered from its persisted copy with its endpoint refusing.
    fn from_the_copy(used: f64) -> Value {
        let mut answer = fetched(used);
        answer["rate_limits"]
            .as_object_mut()
            .unwrap()
            .remove("limits");
        answer
    }

    const NOW: f64 = 1_793_000_000.0;

    #[test]
    fn the_meter() {
        assert_eq!(
            usage(&fetched(71.4), None, NOW),
            Ok(Reading {
                used: 71.4,
                week: "2026-11-04T19:00".to_owned(),
                ends: 1_793_818_800.0,
            })
        );
        // A copy too old to answer from: its endpoint was asked.
        assert!(usage(&fetched(71.4), Some(NOW - SNAPSHOT), NOW).is_ok());
        // Logged in with an API key, or not at all.
        let none =
            json!({"subscription_type": null, "rate_limits_available": false, "rate_limits": null});
        assert!(usage(&none, None, NOW).unwrap_err().contains("logged in"));
    }

    /// `read_fresh` where the copy's time reads from COPIES in turn, the clock
    /// goes on by a minute with every look, and asking is noted.
    fn asked_through(
        copies: Vec<Result<Option<f64>, String>>,
        start: f64,
    ) -> Result<Reading, String> {
        let mut copies = copies.into_iter();
        let mut clock = start;
        let mut asked = false;
        let out = read_fresh(
            &mut || copies.next().unwrap(),
            &mut || {
                clock += 60.0;
                clock - 60.0
            },
            &mut || {
                asked = true;
                Ok(fetched(95.0))
            },
        );
        out.inspect(|_| assert!(asked))
    }

    #[test]
    fn freshness_holds_across_the_asking() {
        let copy = Some(NOW - 59.0);
        // Under SNAPSHOT when asked, past it by the time the answer's in.
        let e = asked_through(vec![Ok(copy), Ok(copy)], NOW).unwrap_err();
        assert!(e.contains("snapshot 59s"), "{e}");
        // Dated ahead of the clock, which then may catch up with it before
        // Claude Code looks.
        let ahead = Some(NOW + 0.5);
        let e = asked_through(vec![Ok(ahead), Ok(ahead)], NOW).unwrap_err();
        assert!(e.contains("snapshot"), "{e}");
        // Past it when asked: it can't have been what answered.
        let old = Some(NOW - 60.0);
        assert!(asked_through(vec![Ok(old), Ok(old)], NOW).is_ok());
        // Nothing there, before or after: its endpoint answered.
        assert!(asked_through(vec![Ok(None), Ok(None)], NOW).is_ok());
        // Replaced while it was asked: that copy may be the answer.
        for after in [Some(NOW + 1.0), None] {
            let e = asked_through(vec![Ok(old), Ok(after)], NOW).unwrap_err();
            assert!(e.contains("changed"), "{e}");
        }
        let e = asked_through(vec![Ok(None), Ok(Some(NOW + 1.0))], NOW).unwrap_err();
        assert!(e.contains("changed"), "{e}");
        // Not known, before or after.
        let unknown = || Err("unreadable".to_owned());
        assert!(asked_through(vec![unknown()], NOW).is_err());
        assert!(asked_through(vec![Ok(old), unknown()], NOW).is_err());
    }

    #[test]
    fn only_a_config_showing_no_copy_counts_as_none() {
        assert_eq!(copied_in(b"{}"), Ok(None));
        assert_eq!(
            copied_in(br#"{"cachedUsageUtilization":{"fetchedAtMs":5000}}"#),
            Ok(Some(5.0))
        );
        assert!(copied_in(b"{\"cachedUsage").is_err());
        assert!(copied_in(br#"{"cachedUsageUtilization":{}}"#).is_err());
    }

    #[test]
    fn a_cached_answer_is_no_reading() {
        let e = usage(&from_the_copy(95.0), Some(NOW - 3000.0), NOW).unwrap_err();
        assert!(e.contains("cached") && e.contains("3000s"), "{e}");
        assert!(usage(&from_the_copy(95.0), None, NOW).is_err());
        let e = usage(&fetched(95.0), Some(NOW - 10.0), NOW).unwrap_err();
        assert!(e.contains("snapshot 10s"), "{e}");
    }

    /// An account whose meter is `get_usage` answers, asked at NOW.
    struct Answers {
        answers: Vec<(Value, Option<f64>)>,
        pressed: usize,
    }

    impl Account for Answers {
        fn read(&mut self) -> Result<Reading, String> {
            let (answer, copied) = self.answers.remove(0);
            usage(&answer, copied, NOW)
        }
        fn reset(&mut self, keep: &mut dyn FnMut() -> Result<(), String>) -> Reset {
            self.pressed += 1;
            keep().unwrap();
            Reset::Pressed("Weekly limit refilled".to_owned())
        }
    }

    fn pass_over(answers: Vec<(Value, Option<f64>)>, state: State) -> (Outcome, usize) {
        let mut account = Answers {
            answers,
            pressed: 0,
        };
        let o = pass(&mut account, &mut |_| Ok(()), state, 2.0, NOW, "now");
        (o, account.pressed)
    }

    #[test]
    fn a_pass_doesnt_trust_a_cached_answer() {
        // Past the line, but as the meter stood up to an hour ago: the week
        // it's from may be over or already refilled. Nothing's pressed.
        let (o, pressed) = pass_over(
            vec![(from_the_copy(99.0), Some(NOW - 3000.0))],
            State::default(),
        );
        assert_eq!(pressed, 0);
        assert!(!o.state.pressed);
        assert_eq!(o.state.failed_reads, 1);

        // Short of the line: its next look isn't booked by a meter this old,
        // nor does its burn count. A fresh 95 with this much burn would be
        // booked later, off the room it seems to have.
        let last = Some((NOW - 3600.0, 90.0));
        let state = State {
            last,
            ..State::default()
        };
        let (o, _) = pass_over(vec![(from_the_copy(95.0), Some(NOW - 3000.0))], state);
        assert_eq!(o.wake, NOW + RETRY);
        assert_eq!(o.state.last, last);
        assert!(o.state.week.is_none());

        // Fresh again, the same meter is believed.
        let (o, pressed) = pass_over(
            vec![(fetched(99.0), Some(NOW - 3000.0)), (fetched(0.0), None)],
            o.state,
        );
        assert_eq!(pressed, 1);
        assert_eq!(o.state.failed_reads, 0);
    }

    #[test]
    fn a_snapshot_from_before_the_yes_doesnt_settle_it() {
        // A copy written while the reset's Claude Code came up, still under
        // SNAPSHOT at the read after its yes, shows the meter from before it.
        let (o, pressed) = pass_over(
            vec![(fetched(99.0), None), (fetched(99.0), Some(NOW - 20.0))],
            State::default(),
        );
        assert_eq!(pressed, 1);
        assert!(keys_of(&o.notices).is_empty(), "{:?}", o.notices);
        assert!(o.state.pressed && o.state.unsettled.is_some());
        assert_eq!(o.wake, NOW + RETRY);
        // The next look, on a fresh answer, sees it took.
        let (o, pressed) = pass_over(vec![(fetched(3.0), None)], o.state);
        assert_eq!(pressed, 0);
        assert_eq!(keys_of(&o.notices)[0], "usage-reset");
        assert!(o.state.unsettled.is_none());
    }

    fn keys_of(notices: &[Notice]) -> Vec<&'static str> {
        notices
            .iter()
            .filter_map(|n| match n {
                Notice::Open(k, ..) => Some(*k),
                Notice::Close(..) => None,
            })
            .collect()
    }
}
