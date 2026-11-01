//! A pass over the repo's probe issues: each open one an author opened gets
//! its answers in one comment and is closed; one anyone else opened gets a
//! line saying no probe ran, and is closed too. A repo GitHub doesn't say is
//! private gets nothing: the answers are the machine's own business.
//!
//! The comment goes up before the close, so an answer is never lost to a
//! close that failed: the issue stays open, and the next pass answers it
//! again.

use burrow_github::{Api, call, encode};
use serde_json::{Value, json};

use crate::query::Query;
use crate::scrub::{Scrubbed, scrub};
use crate::{Config, MARK};

/// The most questions one issue gets answered.
pub const MAX_QUESTIONS: usize = 10;

/// The most a comment holds, under GitHub's 65,536 characters.
const MAX_COMMENT: usize = 60_000;

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
    let mut issues = Vec::new();
    let mut page = Some(format!(
        "repos/{repo}/issues?state=open&labels={}&sort=created&direction=asc&per_page=100",
        encode(&cfg.label)
    ));
    while let Some(path) = page {
        let r = call(api, "GET", &path, None, None).map_err(|e| e.to_string())?;
        page = r.next();
        match r.json().map_err(|e| e.to_string())? {
            Value::Array(items) => issues.extend(items),
            _ => return Err(format!("GitHub's list of {repo}'s issues isn't a list")),
        }
    }
    let mut done = Done::default();
    let mut failed = Vec::new();
    for issue in issues.iter().filter(|i| i.get("pull_request").is_none()) {
        let Some(n) = issue.get("number").and_then(Value::as_u64) else {
            continue;
        };
        let login = issue
            .pointer("/user/login")
            .and_then(Value::as_str)
            .unwrap_or("");
        let author = cfg.authors.iter().any(|a| a.eq_ignore_ascii_case(login));
        let (body, reason) = if author {
            (scrub(&answers(cfg, issue, answer, now)), "completed")
        } else {
            let text = format!(
                "{MARK}\nOnly {} can ask this machine's probe, so nothing was run.",
                cfg.authors.join(" or ")
            );
            (scrub(&text), "not_planned")
        };
        match reply(api, repo, n, &body, reason) {
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

/// The comment, then the close.
fn reply(
    api: &mut dyn Api,
    repo: &str,
    n: u64,
    body: &Scrubbed,
    reason: &str,
) -> Result<(), String> {
    call(
        api,
        "POST",
        &format!("repos/{repo}/issues/{n}/comments"),
        Some(&json!({ "body": body.as_str() })),
        None,
    )
    .map_err(|e| e.to_string())?;
    call(
        api,
        "PATCH",
        &format!("repos/{repo}/issues/{n}"),
        Some(&json!({ "state": "closed", "state_reason": reason })),
        None,
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// The questions an issue asks: each line of its body, or its title when the
/// body asks none. Blank lines, code fences and HTML comments aren't
/// questions; a line's own backticks and a list's `- ` go.
pub fn questions(issue: &Value) -> Vec<String> {
    let lines = |text: &str| -> Vec<String> {
        text.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with("```") && !l.starts_with("<!--"))
            .map(|l| {
                l.strip_prefix("- ")
                    .unwrap_or(l)
                    .trim()
                    .trim_matches('`')
                    .trim()
                    .to_string()
            })
            .filter(|l| !l.is_empty())
            .collect()
    };
    let body = lines(issue.get("body").and_then(Value::as_str).unwrap_or(""));
    if body.is_empty() {
        lines(issue.get("title").and_then(Value::as_str).unwrap_or(""))
    } else {
        body
    }
}

/// The answers to an issue's questions, as one comment.
fn answers(
    cfg: &Config,
    issue: &Value,
    answer: &mut dyn FnMut(&Query) -> Result<String, String>,
    now: &str,
) -> String {
    let asked = questions(issue);
    let mut out = format!("{MARK}\nThe probe on {}, at {now}:\n", cfg.host);
    if asked.is_empty() {
        out += &format!("\nNothing was asked: {}\n", crate::query::VOCABULARY);
        return out;
    }
    let each = MAX_COMMENT / asked.len().min(MAX_QUESTIONS);
    for line in asked.iter().take(MAX_QUESTIONS) {
        let shown = line.replace('`', "'");
        match Query::parse(line) {
            Err(why) => out += &format!("\n**`{shown}`**: {why}\n"),
            Ok(q) => {
                let (text, end) = match answer(&q) {
                    Ok(t) => (t, matches!(q, Query::Journal { .. } | Query::Oom { .. })),
                    Err(why) => (format!("No answer: {why}"), false),
                };
                out += &format!("\n**`{q}`**\n\n{}\n", fenced(&fit(&text, each, end)));
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
