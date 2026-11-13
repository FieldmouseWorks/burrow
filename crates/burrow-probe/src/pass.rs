//! A pass over the repo's probe issues, at its door (burrow-door): each
//! open one an author opened, and only authors wrote, gets its answers in
//! comments and is closed; one anyone else wrote gets a line saying no
//! probe ran, and is closed too. A repo GitHub doesn't say is private gets
//! nothing: the answers are the machine's own business.

use burrow_door::{Door, MAX_COMMENT, asked, clip, fenced};
use burrow_github::{Api, Error, call};
use serde_json::Value;

use crate::query::Query;
use crate::scrub::{REDACTED, redact_keys, scrub};
use crate::{Config, MARK};

/// The most questions one issue gets answered.
pub const MAX_QUESTIONS: usize = 10;

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
) -> Result<Done, Error> {
    let repo = &cfg.repo;
    let about = call(api, "GET", &format!("repos/{repo}"), None, None).and_then(|r| r.json())?;
    if about.get("private").and_then(Value::as_bool) != Some(true) {
        return Err(format!(
            "{repo} isn't private, so no probe is answered there: its answers are the machine's own"
        )
        .into());
    }
    let mut authors = cfg.authors.clone();
    authors.extend(cfg.machinery.iter().cloned());
    let door = Door {
        repo,
        label: &cfg.label,
        authors: &authors,
        what: "this machine's probe",
    };
    let mut done = Done::default();
    let mut failed = Vec::new();
    for issue in door.issues(api)? {
        let n = issue.number;
        let vetted = match door.written(api, &issue) {
            Ok(v) => v,
            Err(e) => {
                failed.push(e.context(format!("#{n}")));
                continue;
            }
        };
        let author = vetted.is_ok();
        let (body, reason) = match vetted {
            Ok(text) => (
                answers(cfg, &text, answer, now)
                    .iter()
                    .map(|p| scrub(p))
                    .collect::<Vec<_>>(),
                "completed",
            ),
            Err(why) => (vec![scrub(&format!("{MARK}\n{why}"))], "not_planned"),
        };
        // Post every page before closing. A failed continuation leaves the
        // issue open, just as a failed single-comment answer does.
        let reply = (|| {
            for page in &body[..body.len() - 1] {
                call(
                    api,
                    "POST",
                    &format!("repos/{repo}/issues/{n}/comments"),
                    Some(&serde_json::json!({"body": page.as_str()})),
                    None,
                )?;
            }
            door.reply(api, n, body.last().unwrap().as_str(), reason)
        })();
        match reply {
            Ok(()) if author => done.answered.push(n),
            Ok(()) => done.refused.push(n),
            Err(e) => failed.push(e.context(format!("#{n}"))),
        }
    }
    if failed.is_empty() {
        Ok(done)
    } else {
        Err(Error::aggregate(failed).context(repo))
    }
}

/// Answers packed into comments; required usage sections are never clipped.
fn answers(
    cfg: &Config,
    issue: &Value,
    answer: &mut dyn FnMut(&Query) -> Result<String, String>,
    now: &str,
) -> Vec<String> {
    // Take each complete request field's private keys out before
    // normalization, question limits, clipping or pagination can separate a
    // key's markers from its payload. Only keys: the questions run as asked,
    // and what's echoed of them is scrubbed in context below.
    let request = serde_json::json!({
        "title": redact_keys(issue["title"].as_str().unwrap_or("")),
        "body": redact_keys(issue["body"].as_str().unwrap_or("")),
    });
    let asked = asked(&request);
    let header = scrub(&format!(
        "{MARK}\nThe probe on {}, at {now}:\n",
        clip(&cfg.host, MAX_ECHO)
    ))
    .as_str()
    .to_string();
    let mut pages = vec![header.clone()];
    if asked.is_empty() {
        *pages.last_mut().unwrap() +=
            &format!("\nNothing was asked: {}\n", crate::query::VOCABULARY);
        return pages;
    }
    // Optional answers keep their per-question budget. Usage's required
    // prefix gets all the space it needs, across comments when necessary.
    let share = (MAX_COMMENT - 1_000) / asked.len().min(MAX_QUESTIONS);
    let each = share.saturating_sub(4 * MAX_ECHO + 100);
    for (i, line) in asked.iter().enumerate().take(MAX_QUESTIONS) {
        let before = i.checked_sub(1).map(|b| asked[b].as_str());
        let echoed = echo(before, line);
        let shown = clip(&echoed, MAX_ECHO).replace('`', "'");
        match Query::parse(line) {
            // A refusal's reason quotes the line, so one that looks like a
            // secret, alone or after the line before it, gets no reason.
            Err(_) if echoed != *line => append(
                &mut pages,
                &header,
                scrub(&format!(
                    "\n**`{shown}`**: not a question, and it looks like it holds a secret ({})\n",
                    crate::query::VOCABULARY
                ))
                .as_str(),
            ),
            Err(why) => append(
                &mut pages,
                &header,
                scrub(&format!("\n**`{shown}`**: {}\n", clip(&why, MAX_ECHO))).as_str(),
            ),
            Ok(q) => {
                let (text, end) = match answer(&q) {
                    Ok(t) => (t, matches!(q, Query::Journal { .. } | Query::Oom { .. })),
                    Err(why) => (format!("No answer: {why}"), false),
                };
                // Scrubbed whole before it's cut, so a cut can't leave part
                // of a secret its rule needed all of to know (a key's END
                // without its BEGIN, say). Headings and refusals are scrubbed too.
                let text = scrub(&text).as_str().to_string();
                let heading = clip(&echo(before, &q.to_string()), MAX_ECHO).replace('`', "'");
                let text = if matches!(q, Query::Usage { .. }) {
                    // --totals has no optional suffix. Otherwise only the
                    // jobs, meter readings and per-turn details share the budget.
                    match text.split_once(crate::answer::USAGE_DETAIL) {
                        Some((required, rest)) => format!(
                            "{required}{}",
                            fit(
                                &format!("{}{rest}", crate::answer::USAGE_DETAIL),
                                each.saturating_sub(required.len()),
                                false
                            )
                        ),
                        None => text,
                    }
                } else {
                    fit(&text, each, end)
                };
                append_answer(&mut pages, &header, &heading, &text);
            }
        }
    }
    if asked.len() > MAX_QUESTIONS {
        append(
            &mut pages,
            &header,
            &format!(
                "\nOnly the first {MAX_QUESTIONS} questions are answered; ask the other {} in another issue.\n",
                asked.len() - MAX_QUESTIONS
            ),
        );
    }
    pages
}

/// `line` as it may be echoed: scrubbed after the question `before` it,
/// since a secret's shape can start on one line and end on the next
/// (`password:`, then its value). If a rule took the line break with it,
/// the whole line is hidden.
fn echo(before: Option<&str>, line: &str) -> String {
    let Some(before) = before else {
        return scrub(line).as_str().to_string();
    };
    let both = scrub(&format!("{before}\n{line}"));
    match both.as_str().split_once('\n') {
        Some((_, after)) if !after.contains('\n') => after.to_string(),
        _ => REDACTED.to_string(),
    }
}

fn append(pages: &mut Vec<String>, header: &str, fragment: &str) {
    if pages.last().unwrap().len() + fragment.len() > MAX_COMMENT {
        pages.push(format!("{header}(continued)\n"));
    }
    *pages.last_mut().unwrap() += fragment;
}

/// Fence each chunk independently, reserving up to twice its text size for
/// fences (the worst case is a chunk consisting entirely of backticks).
/// Split at lines where possible; a single oversized line splits at UTF-8
/// boundaries. Never pass an oversized required answer to Door's clipping.
fn append_answer(pages: &mut Vec<String>, header: &str, heading: &str, mut text: &str) {
    let prefix = format!("\n**`{heading}`**\n\n");
    let budget = MAX_COMMENT.saturating_sub(header.len() + prefix.len() + 100);
    loop {
        let mut end = text.len().min(budget / 3);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        if end < text.len()
            && let Some(line) = text[..end].rfind('\n')
        {
            end = line + 1;
        }
        let fragment = format!("{prefix}{}\n", fenced(&text[..end]));
        append(pages, header, &fragment);
        text = &text[end..];
        if text.is_empty() {
            break;
        }
    }
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
