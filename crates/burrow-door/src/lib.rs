//! burrow-door: a door on a repo's issues, for a program that does what an
//! issue asks with no Claude in between (the probe, the ledger's undo). An
//! open issue with the door's label, opened by one of its authors, is a
//! request; the program answers it with one comment and closes it.
//! Reopening it asks again.
//!
//! What's done is the issue's text as GitHub has it now, and only when its
//! opener and everyone who has changed its body or title is an author:
//! GitHub lets a collaborator edit anyone's issue, so who opened it says
//! nothing about who wrote what it asks.
//!
//! The comment goes up before the close, so an answer is never lost to a
//! close that failed: the issue stays open, and the next pass answers it
//! again.

use burrow_github::{Api, Error, call, encode, query};
use serde_json::{Value, json};

/// An issue's text, and everyone who wrote it: its opener, each version of
/// its body, and each change of its title. In one query, so the text that
/// runs is the text whose writers were checked.
const WRITTEN: &str = "query($owner: String!, $name: String!, $n: Int!) {
  repository(owner: $owner, name: $name) {
    issue(number: $n) {
      id
      author { login }
      title
      body
      openCycle: timelineItems(itemTypes: [REOPENED_EVENT], last: 1) {
        nodes { ... on ReopenedEvent { id } }
      }
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

/// The door: a repo's issues with a label, from its authors.
pub struct Door<'a> {
    /// OWNER/NAME.
    pub repo: &'a str,
    pub label: &'a str,
    /// Who may ask (GitHub logins, any case).
    pub authors: &'a [String],
    /// What they ask, as a refusal names it: "this machine's probe".
    pub what: &'a str,
}

/// One open issue at the door, as its list gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub number: u64,
    /// Who opened it.
    pub opener: String,
}

impl Door<'_> {
    /// The open issues with the label, oldest first: PRs aren't issues.
    pub fn issues(&self, api: &mut dyn Api) -> Result<Vec<Issue>, Error> {
        let repo = self.repo;
        let mut items = Vec::new();
        let mut page = Some(format!(
            "repos/{repo}/issues?state=open&labels={}&sort=created&direction=asc&per_page=100",
            encode(self.label)
        ));
        while let Some(path) = page {
            let r = call(api, "GET", &path, None, None)?;
            page = r.next();
            match r.json()? {
                Value::Array(more) => items.extend(more),
                _ => return Err(format!("GitHub's list of {repo}'s issues isn't a list").into()),
            }
        }
        Ok(items
            .iter()
            .filter(|i| i.get("pull_request").is_none())
            .filter_map(|i| {
                Some(Issue {
                    number: i.get("number").and_then(Value::as_u64)?,
                    opener: i
                        .pointer("/user/login")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                })
            })
            .collect())
    }

    pub fn is_author(&self, login: &str) -> bool {
        self.authors.iter().any(|a| a.eq_ignore_ascii_case(login))
    }

    /// The issue's title and body as GitHub has them now (`{title, body}`),
    /// when an author opened it and every edit of either was an author's;
    /// else, as Err inside, why nothing runs, to say on it. An editor GitHub
    /// doesn't name, or more edits than one query reads, isn't known to be
    /// an author's.
    pub fn written(
        &self,
        api: &mut dyn Api,
        issue: &Issue,
    ) -> Result<Result<Value, String>, Error> {
        self.vetted(api, issue)
            .map(|r| r.map(|i| json!({"title": i["title"], "body": i["body"]})))
    }

    /// The vetted text and its open cycle, in the same snapshot. A reopen
    /// makes a fresh request even when the title and body did not change.
    pub fn written_cycle(
        &self,
        api: &mut dyn Api,
        issue: &Issue,
    ) -> Result<Result<(Value, String), String>, Error> {
        self.vetted(api, issue).map(|r| {
            r.and_then(|i| {
                let nodes = i["openCycle"]["nodes"]
                    .as_array()
                    .ok_or("GitHub didn't identify the issue's open cycle")?;
                let cycle = match nodes.as_slice() {
                    [] => "initial".to_owned(),
                    [event] => event["id"]
                        .as_str()
                        .ok_or("GitHub didn't identify the reopen event")?
                        .to_owned(),
                    _ => return Err("GitHub returned more than one latest reopen event".into()),
                };
                let issue = i["id"].as_str().ok_or("GitHub didn't identify the issue")?;
                Ok((
                    json!({"title": i["title"], "body": i["body"]}),
                    format!("{issue}:{cycle}"),
                ))
            })
        })
    }

    fn vetted(&self, api: &mut dyn Api, issue: &Issue) -> Result<Result<Value, String>, Error> {
        let who = self.authors.join(" or ");
        if !self.is_author(&issue.opener) {
            return Ok(Err(format!(
                "Only {who} can ask {}, so nothing was run.",
                self.what
            )));
        }
        let (owner, name) = self
            .repo
            .split_once('/')
            .ok_or_else(|| format!("{} isn't OWNER/NAME", self.repo))?;
        let data = query(
            api,
            owner,
            WRITTEN,
            json!({ "owner": owner, "name": name, "n": issue.number }),
        )?;
        let issue = &data["repository"]["issue"];
        let by_author = |v: &Value| v.as_str().is_some_and(|l| self.is_author(l));
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
                "This issue's text was written or changed by someone other than {who}, or by someone GitHub doesn't name, so nothing was run. Ask in a new issue."
            )));
        }
        Ok(Ok(issue.clone()))
    }

    /// The comment, cut to what one holds ([`MAX_COMMENT`]), then the
    /// close, as `reason` (GitHub's `state_reason`: `completed`,
    /// `not_planned`). A comment longer than GitHub takes would be refused
    /// on every pass, its issue answered never; cut, it's answered once.
    pub fn reply(&self, api: &mut dyn Api, n: u64, body: &str, reason: &str) -> Result<(), Error> {
        let repo = self.repo;
        call(
            api,
            "POST",
            &format!("repos/{repo}/issues/{n}/comments"),
            Some(&json!({ "body": bounded(body) })),
            None,
        )?;
        call(
            api,
            "PATCH",
            &format!("repos/{repo}/issues/{n}"),
            Some(&json!({ "state": "closed", "state_reason": reason })),
            None,
        )?;
        Ok(())
    }
}

/// The lines an issue asks: each line of its body, or its title when the
/// body asks none. Blank lines, code fences and HTML comments aren't asked;
/// a line's own backticks and a list's `- ` go. A rule (`---`) ends them:
/// what's under it is a signature, as the footer Claude Code puts on
/// everything it posts.
pub fn asked(issue: &Value) -> Vec<String> {
    // CommonMark's thematic break: three or more of one marker, with only
    // spaces or tabs between.
    let rule = |l: &str| {
        let mut marks = l.chars().filter(|c| !matches!(c, ' ' | '\t'));
        marks.next().is_some_and(|m| {
            matches!(m, '-' | '*' | '_') && {
                let rest: Vec<char> = marks.collect();
                rest.len() >= 2 && rest.iter().all(|&c| c == m)
            }
        })
    };
    let lines = |text: &str| -> Vec<String> {
        text.lines()
            .map(str::trim)
            .take_while(|l| !rule(l))
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

/// `text`'s first `max` characters, saying when there were more.
/// The most a door's comment holds, in bytes, under GitHub's 65,536
/// characters.
pub const MAX_COMMENT: usize = 60_000;

/// What a comment cut to [`MAX_COMMENT`] ends with.
const CUT: &str = "\n\n(cut here, past what a comment holds)";

/// `body`, at most [`MAX_COMMENT`] bytes, cut at a character's end. Only
/// ever shorter, so what a cut leaves was scrubbed with the rest.
pub fn bounded(body: &str) -> String {
    if body.len() <= MAX_COMMENT {
        return body.to_owned();
    }
    let mut at = MAX_COMMENT - CUT.len();
    while !body.is_char_boundary(at) {
        at -= 1;
    }
    format!("{}{CUT}", &body[..at])
}

pub fn clip(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((at, _)) => format!(
            "{}... ({} more characters)",
            &text[..at],
            text[at..].chars().count()
        ),
        None => text.to_string(),
    }
}

/// `text` in a code block whose fence no run of backticks in it can close.
pub fn fenced(text: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    let fence = "`".repeat((longest + 1).max(3));
    format!("{fence}text\n{}\n{fence}", text.trim_end())
}

#[cfg(test)]
mod tests;
