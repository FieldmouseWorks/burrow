//! The account through Claude Code itself, as the machine's user: the meter
//! from its SDK's `get_usage` request, and the reset through its own
//! `/limit-reset weekly`, typed into a Claude Code of our own in a tmux, the
//! way the owner would at a terminal. It keys nothing but that command, the
//! folder-trust answer for its own folder and the confirmation of the reset,
//! each only once the screen shows what it answers; anything else ends the
//! session, and ending Claude Code never uses a reset.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::pass::{Account, Reading, Reset};

/// How long Claude Code has to answer, or to show the next screen.
const TIMEOUT: Duration = Duration::from_secs(60);
/// How often the screen is looked at while waiting on it.
const POLL: Duration = Duration::from_millis(500);

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
        usage(&self.ask_usage()?)
    }

    fn reset(&mut self) -> Reset {
        let tmux = Tmux {
            bin: self.tmux.clone(),
        };
        let outcome = self.drive(&tmux);
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

    fn drive(&self, tmux: &Tmux) -> Reset {
        if let Err(e) = tmux.start(&self.dir, &self.claude) {
            return Reset::NotPressed(e);
        }
        // Up to its input box, answering only its question about trusting
        // its own folder, once.
        let mut trusted = false;
        loop {
            let seen = tmux.wait(|screen| {
                if prompt_ready(screen) {
                    Some(true)
                } else {
                    (!trusted && asks_trust(screen)).then_some(false)
                }
            });
            match seen {
                Ok(true) => break,
                Ok(false) => {
                    trusted = true;
                    if let Err(e) = tmux.keys(&["Enter"]) {
                        return Reset::NotPressed(e);
                    }
                }
                Err(e) => return Reset::NotPressed(format!("Claude Code didn't come up: {e}")),
            }
        }

        if let Err(e) = tmux.literal(COMMAND).and_then(|()| tmux.keys(&["Enter"])) {
            return Reset::NotPressed(e);
        }
        // Its answer: a confirmation to give, or a line saying why there's
        // nothing to confirm.
        for _ in 0..4 {
            let seen = tmux.wait(|screen| match read_reset(screen) {
                Screen::Busy => None,
                other => Some(other),
            });
            match seen {
                Err(e) => return Reset::NotPressed(format!("no answer to {COMMAND}: {e}")),
                Ok(Screen::Busy) => unreachable!(),
                Ok(Screen::Said(r)) => return r,
                Ok(Screen::Confirm(Cursor::OnYes)) => {
                    if let Err(e) = tmux.keys(&["Enter"]) {
                        return Reset::NotPressed(e);
                    }
                    return self.after(tmux);
                }
                Ok(Screen::Confirm(cursor)) => {
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
            }
        }
        Reset::NotPressed("couldn't put the cursor on the reset's yes".to_owned())
    }

    /// After the confirmation: what Claude Code said it did.
    fn after(&self, tmux: &Tmux) -> Reset {
        let said = tmux.wait(|screen| {
            let said = said(screen);
            (!said.is_empty() && !matches!(read_reset(screen), Screen::Confirm(_))).then_some(said)
        });
        Reset::Pressed(said.unwrap_or_else(|e| format!("it showed nothing after: {e}")))
    }
}

/// The weekly meter in a `get_usage` answer.
pub fn usage(answer: &Value) -> Result<Reading, String> {
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
    // To the minute: the same week's end may come back with a different
    // fraction of a second.
    Ok(Reading {
        used,
        week: resets.chars().take(16).collect(),
    })
}

/// What it types.
const COMMAND: &str = "/limit-reset weekly";
/// The confirmation's yes, as Claude Code words it unless its server says
/// otherwise: "Yes, use my weekly reset".
const YES: &str = "Yes, use my";

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
    let lines = answer(screen);
    if let Some(yes) = lines.iter().position(|l| l.contains(YES)) {
        // The choices' cursor, not the input box's (a `❯` under a rule).
        let cursor = (0..lines.len())
            .find(|&i| lines[i].trim_start().starts_with('❯') && !(i > 0 && is_rule(lines[i - 1])));
        return Screen::Confirm(match cursor {
            Some(c) if c == yes => Cursor::OnYes,
            Some(c) if c > yes => Cursor::Below,
            // No cursor drawn yet reads as above: Down puts it on the first
            // choice, and the next look sees where.
            _ => Cursor::Above,
        });
    }
    let text = lines.join("\n");
    let has = |s: &str| text.contains(s);
    let said = said(screen);
    // Claude Code's own lines (Claude Code 2.1.314, limit-reset): the ones
    // that settle it without using anything.
    if has("only works at a usage limit")
        || has("saved for when you reach a usage limit")
        || has("at a limit to use it")
    {
        Screen::Said(Reset::AtLimitOnly(said))
    } else if has("No reset to use")
        || has("No weekly reset to use")
        || has("Unknown command")
        || has("Unknown slash command")
    {
        Screen::Said(Reset::NoneBanked(said))
    } else if has("nothing was used") || has("can't be used") || has("Couldn't") || has(" kept") {
        Screen::Said(Reset::NotPressed(said))
    } else {
        Screen::Busy
    }
}

/// The lines under COMMAND's echo: what Claude Code answered it with, and
/// none of the notices it started with. The whole screen until it echoes.
fn answer(screen: &str) -> Vec<&str> {
    let lines: Vec<&str> = screen.lines().collect();
    match lines.iter().rposition(|l| l.trim_end().ends_with(COMMAND)) {
        Some(i) => lines[i + 1..].to_vec(),
        None => lines,
    }
}

/// Its answer's words, as one line for a notice.
fn said(screen: &str) -> String {
    let words: Vec<&str> = answer(screen)
        .into_iter()
        .map(|l| l.trim().trim_start_matches(['⎿', '❯', '●', ' ']))
        .filter(|l| {
            !l.is_empty()
                && !l.chars().all(|c| "─▔━".contains(c))
                && !l.contains("for shortcuts")
                && !l.contains("bypass permissions")
        })
        .collect();
    words.join(" / ").chars().take(300).collect()
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

/// It's asking whether to trust the folder it was started in, with the
/// cursor on yes.
fn asks_trust(screen: &str) -> bool {
    screen.to_lowercase().contains("trust")
        && !prompt_ready(screen)
        && screen
            .lines()
            .any(|l| l.trim_start().starts_with('❯') && l.contains("Yes"))
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
        let trust = "
 Do you trust the files in this folder?

 /var/lib/burrow/usage-reset/claude

 ❯ 1. Yes, I trust this folder
   2. No, exit
";
        assert!(asks_trust(trust) && !prompt_ready(trust));
        assert!(!asks_trust(
            &trust.replace("❯ 1", "  1").replace("  2. No", "❯ 2. No")
        ));
    }

    #[test]
    fn only_its_answer_counts() {
        // The notices before COMMAND (a kept line among them) settle nothing.
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
            said("Unknown command: /limit-reset"),
            Screen::Said(Reset::NoneBanked(_))
        ));
    }

    #[test]
    fn what_it_said_after() {
        let done = answered(
            "  ⎿  Weekly reset used · your weekly reset day stays Wednesday · 0 weekly resets left",
        );
        assert!(!matches!(read_reset(&done), Screen::Confirm(_)));
        assert_eq!(
            said(&done),
            "Weekly reset used · your weekly reset day stays Wednesday · 0 weekly resets left"
        );
    }

    #[test]
    fn the_meter() {
        let answer = json!({
            "session": {"total_cost_usd": 0},
            "subscription_type": "max",
            "rate_limits_available": true,
            "rate_limits": {
                "five_hour": {"utilization": 35.0, "resets_at": "2026-10-30T06:40:00.412Z"},
                "seven_day": {"utilization": 71.4, "resets_at": "2026-11-04T19:00:00.412Z"}
            },
            "behaviors": null
        });
        assert_eq!(
            usage(&answer),
            Ok(Reading {
                used: 71.4,
                week: "2026-11-04T19:00".to_owned()
            })
        );
        // Logged in with an API key, or not at all.
        let none =
            json!({"subscription_type": null, "rate_limits_available": false, "rate_limits": null});
        assert!(usage(&none).unwrap_err().contains("logged in"));
    }
}
