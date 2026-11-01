//! A pass over the repo's probe issues: each open one an author opened gets
//! its answers in one comment and is closed; one anyone else opened gets a
//! line saying no probe ran, and is closed too. A repo GitHub doesn't say is
//! private gets nothing: the answers are the machine's own business.
//!
//! What runs is the issue's text as GitHub has it now, and only when its
//! opener and everyone who has changed its body or title is an author:
//! GitHub lets a collaborator edit anyone's issue, so who opened it says
//! nothing about who wrote what it asks.
//!
//! The comment goes up before the close, so an answer is never lost to a
//! close that failed: the issue stays open, and the next pass answers it
//! again.

use burrow_github::{Api, call, encode, query};
use serde_json::{Value, json};

use crate::query::Query;
use crate::scrub::{Scrubbed, scrub};
use crate::{Config, MARK};

/// The most questions one issue gets answered.
pub const MAX_QUESTIONS: usize = 10;

/// The most a comment holds, under GitHub's 65,536 characters.
const MAX_COMMENT: usize = 60_000;

/// The most of a question, or of why it isn't one, a comment echoes.
const MAX_ECHO: usize = 200;

/// An issue's text, and everyone who wrote it: its opener, each version of
/// its body, and each change of its title. In one query, so the text that
/// runs is the text whose writers were checked.
const WRITTEN: &str = "query($owner: String!, $name: String!, $n: Int!) {
  repository(owner: $owner, name: $name) {
    issue(number: $n) {
      author { login }
      title
      body
      userContentEdits(first: 100) {
        totalCount
        nodes { editor { login } deletedBy { login } }
      }
      timelineItems(itemTypes: [RENAMED_TITLE_EVENT], first: 100) {
        totalCount
        nodes { ... on RenamedTitleEvent { actor { login } } }
      }
    }
  }
}";

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
        let vetted = if is_author(cfg, login) {
            match written(api, cfg, n) {
                Ok(v) => v,
                Err(e) => {
                    failed.push(format!("#{n}: {e}"));
                    continue;
                }
            }
        } else {
            Err(format!(
                "Only {} can ask this machine's probe, so nothing was run.",
                cfg.authors.join(" or ")
            ))
        };
        let author = vetted.is_ok();
        let (body, reason) = match vetted {
            Ok(text) => (
                bounded(scrub(&answers(cfg, &text, answer, now))),
                "completed",
            ),
            Err(why) => (scrub(&format!("{MARK}\n{why}")), "not_planned"),
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

fn is_author(cfg: &Config, login: &str) -> bool {
    cfg.authors.iter().any(|a| a.eq_ignore_ascii_case(login))
}

/// Issue `n`'s title and body as GitHub has them now, when an author opened
/// it and every edit of either was an author's; else, as Err inside, why
/// nothing runs. An editor GitHub doesn't name, or more edits than one query
/// reads, isn't known to be an author's.
fn written(api: &mut dyn Api, cfg: &Config, n: u64) -> Result<Result<Value, String>, String> {
    let (owner, name) = cfg
        .repo
        .split_once('/')
        .ok_or_else(|| format!("{} isn't OWNER/NAME", cfg.repo))?;
    let data = query(
        api,
        owner,
        WRITTEN,
        json!({ "owner": owner, "name": name, "n": n }),
    )
    .map_err(|e| e.to_string())?;
    let issue = &data["repository"]["issue"];
    let by_author = |v: &Value| v.as_str().is_some_and(|l| is_author(cfg, l));
    let every = |list: &Value, ok: &dyn Fn(&Value) -> bool| {
        list["nodes"].as_array().is_some_and(|nodes| {
            list["totalCount"].as_u64() == Some(nodes.len() as u64) && nodes.iter().all(ok)
        })
    };
    let edits = every(&issue["userContentEdits"], &|e| {
        by_author(&e["editor"]["login"])
            && (e["deletedBy"].is_null() || by_author(&e["deletedBy"]["login"]))
    });
    let renames = every(&issue["timelineItems"], &|r| {
        by_author(&r["actor"]["login"])
    });
    if !by_author(&issue["author"]["login"]) || !edits || !renames {
        return Ok(Err(format!(
            "This issue's text was written or changed by someone other than {}, or by someone GitHub doesn't name, so nothing was run. Ask in a new issue.",
            cfg.authors.join(" or ")
        )));
    }
    Ok(Ok(
        json!({ "title": issue["title"], "body": issue["body"] }),
    ))
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
