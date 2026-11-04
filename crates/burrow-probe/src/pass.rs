//! A pass over the repo's probe issues, at its door (burrow-door): each
//! open one an author opened, and only authors wrote, gets its answers in
//! one comment and is closed; one anyone else wrote gets a line saying no
//! probe ran, and is closed too. A repo GitHub doesn't say is private gets
//! nothing: the answers are the machine's own business.

use burrow_door::{Door, asked};
use burrow_github::{Api, call};
use serde_json::Value;

use crate::query::Query;
use crate::scrub::{Scrubbed, scrub};
use crate::{Config, MARK};

/// The most questions one issue gets answered.
pub const MAX_QUESTIONS: usize = 10;

/// The most a comment holds, under GitHub's 65,536 characters.
const MAX_COMMENT: usize = 60_000;

/// The most of a question, or of why it isn't one, a comment echoes.
const MAX_ECHO: usize = 200;

/// What a pass did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Done {
    /// The issues answered, and turned away.
    pub answered: Vec<u64>,
    pub refused: Vec<u64>,
}

/// One pass. `answer` gives each question's answer as the machine says it;
/// `now` is the answer's time, as a person reads it.
pub fn pass(
    api: &mut dyn Api,
    cfg: &Config,
    answer: &mut dyn FnMut(&Query) -> Result<String, String>,
    now: &str,
) -> Result<Done, String> {
    let repo = &cfg.repo;
    let about = call(api, "GET", &format!("repos/{repo}"), None, None)
        .and_then(|r| r.json())
        .map_err(|e| e.to_string())?;
    if about.get("private").and_then(Value::as_bool) != Some(true) {
        return Err(format!(
            "{repo} isn't private, so no probe is answered there: its answers are the machine's own"
        ));
    }
    let door = Door {
        repo,
        label: &cfg.label,
        authors: &cfg.authors,
        what: "this machine's probe",
    };
    let mut done = Done::default();
    let mut failed = Vec::new();
    for issue in door.issues(api)? {
        let n = issue.number;
        let vetted = match door.written(api, &issue) {
            Ok(v) => v,
            Err(e) => {
                failed.push(format!("#{n}: {e}"));
                continue;
            }
        };
        let author = vetted.is_ok();
        let (body, reason) = match vetted {
            Ok(text) => (
                bounded(scrub(&answers(cfg, &text, answer, now))),
                "completed",
            ),
            Err(why) => (scrub(&format!("{MARK}\n{why}")), "not_planned"),
        };
        match door.reply(api, n, body.as_str(), reason) {
            Ok(()) if author => done.answered.push(n),
            Ok(()) => done.refused.push(n),
            Err(e) => failed.push(format!("#{n}: {e}")),
        }
    }
    if failed.is_empty() {
        Ok(done)
    } else {
        Err(format!("{repo}: {}", failed.join("; ")))
    }
}

/// A comment cut to what one holds, whatever made it long: answers are fit
/// to their share already, so this is for what nothing else bounds.
fn bounded(body: Scrubbed) -> Scrubbed {
    body.cut(MAX_COMMENT, "\n\n(cut here, past what a comment holds)")
}

/// `text`'s first `max` characters, saying when there were more.
fn clip(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((at, _)) => format!(
            "{}... ({} more characters)",
            &text[..at],
            text[at..].chars().count()
        ),
        None => text.to_string(),
    }
}

/// The answers to an issue's questions, as one comment.
fn answers(
    cfg: &Config,
    issue: &Value,
    answer: &mut dyn FnMut(&Query) -> Result<String, String>,
    now: &str,
) -> String {
    let asked = asked(issue);
    let mut out = format!("{MARK}\nThe probe on {}, at {now}:\n", cfg.host);
    if asked.is_empty() {
        out += &format!("\nNothing was asked: {}\n", crate::query::VOCABULARY);
        return out;
    }
    // Each answer's share of the comment, less what goes around it: the
    // heading above, its question (clipped, up to four bytes a character),
    // its fence and a note of a cut. The comment as a whole is cut too, for
    // what this doesn't foresee (a fence a run of backticks made long).
    let share = (MAX_COMMENT - 1_000) / asked.len().min(MAX_QUESTIONS);
    let each = share.saturating_sub(4 * MAX_ECHO + 100);
    for line in asked.iter().take(MAX_QUESTIONS) {
        let shown = clip(line, MAX_ECHO).replace('`', "'");
        match Query::parse(line) {
            Err(why) => out += &format!("\n**`{shown}`**: {}\n", clip(&why, MAX_ECHO)),
            Ok(q) => {
                let (text, end) = match answer(&q) {
                    Ok(t) => (t, matches!(q, Query::Journal { .. } | Query::Oom { .. })),
                    Err(why) => (format!("No answer: {why}"), false),
                };
                // Scrubbed whole before it's cut, so a cut can't leave part
                // of a secret its rule needed all of to know (a key's END
                // without its BEGIN, say). The comment is scrubbed again.
                let text = scrub(&text);
                let heading = clip(&q.to_string(), MAX_ECHO).replace('`', "'");
                out += &format!(
                    "\n**`{heading}`**\n\n{}\n",
                    fenced(&fit(text.as_str(), each, end))
                );
            }
        }
    }
    if asked.len() > MAX_QUESTIONS {
        out += &format!(
            "\nOnly the first {MAX_QUESTIONS} questions are answered; ask the other {} in another issue.\n",
            asked.len() - MAX_QUESTIONS
        );
    }
    out
}

/// `text` in a code block whose fence no run of backticks in it can close.
fn fenced(text: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    let fence = "`".repeat((longest + 1).max(3));
    format!("{fence}text\n{}\n{fence}", text.trim_end())
}

/// `text` cut to `max` bytes, at a line's end: its last lines when `end`
/// (a journal's newest), else its first, saying so.
fn fit(text: &str, max: usize, end: bool) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let lines: Vec<&str> = text.lines().collect();
    let mut kept = Vec::new();
    let mut size = 0;
    let order: Box<dyn Iterator<Item = &&str>> = if end {
        Box::new(lines.iter().rev())
    } else {
        Box::new(lines.iter())
    };
    for l in order {
        if size + l.len() + 1 > max.saturating_sub(100) {
            break;
        }
        size += l.len() + 1;
        kept.push(*l);
    }
    let cut = lines.len() - kept.len();
    if end {
        kept.reverse();
        format!(
            "({cut} earlier lines cut, past what a comment holds)\n{}",
            kept.join("\n")
        )
    } else {
        format!(
            "{}\n({cut} more lines cut, past what a comment holds)",
            kept.join("\n")
        )
    }
}

#[cfg(test)]
#[path = "pass_tests.rs"]
mod tests;
