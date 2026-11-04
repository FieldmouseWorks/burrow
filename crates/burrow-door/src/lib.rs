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

use burrow_github::{Api, call, encode, query};
use serde_json::{Value, json};

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
    pub fn issues(&self, api: &mut dyn Api) -> Result<Vec<Issue>, String> {
        let repo = self.repo;
        let mut items = Vec::new();
        let mut page = Some(format!(
            "repos/{repo}/issues?state=open&labels={}&sort=created&direction=asc&per_page=100",
            encode(self.label)
        ));
        while let Some(path) = page {
            let r = call(api, "GET", &path, None, None).map_err(|e| e.to_string())?;
            page = r.next();
            match r.json().map_err(|e| e.to_string())? {
                Value::Array(more) => items.extend(more),
                _ => return Err(format!("GitHub's list of {repo}'s issues isn't a list")),
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
    ) -> Result<Result<Value, String>, String> {
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
        )
        .map_err(|e| e.to_string())?;
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
        Ok(Ok(
            json!({ "title": issue["title"], "body": issue["body"] }),
        ))
    }

    /// The comment, then the close, as `reason` (GitHub's `state_reason`:
    /// `completed`, `not_planned`).
    pub fn reply(&self, api: &mut dyn Api, n: u64, body: &str, reason: &str) -> Result<(), String> {
        let repo = self.repo;
        call(
            api,
            "POST",
            &format!("repos/{repo}/issues/{n}/comments"),
            Some(&json!({ "body": body })),
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

#[cfg(test)]
mod tests;
