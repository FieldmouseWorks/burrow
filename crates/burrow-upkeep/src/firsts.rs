//! Which of a repo's open PRs go first (labels.first): ahead in its line for
//! proofSlots and in the pass's order (the steward's proofs.py and
//! steward.py). One does when an account of `first_by` put `first` on it,
//! or on an issue it closes; this is the one place a PR gets the label from
//! its issue, put on here so the PR carries what its issue does. proj-inbox
//! reads the same label on the issue (crates/burrow-proj/src/inbox/first.rs).
//!
//! Who put a label on is read from GitHub's events for the issue or PR: the
//! actor of the last `labeled` event naming it, by account id, never any
//! text. `first_by` is proj's inbox authors and the steward's own App; the
//! machinery's App, which Claude jobs act as, isn't one of them, so a job
//! can't move its own work up the line. It changes order only.
//!
//! The steward hands over each PR as its pass's query read it: its labels
//! and the issues it closes, each with its labels. A connection GitHub cut
//! short is read whole here before anything is concluded from it.

use burrow_github::call;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::pass::Pass;

/// The open PRs, as the steward's query has them.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ask {
    pub prs: Vec<Pr>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Pr {
    pub number: u64,
    #[serde(default)]
    pub labels: Connection<Label>,
    /// None when the query didn't read them: it closes none, then.
    #[serde(default)]
    pub closing_issues_references: Option<Connection<Issue>>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Connection<T> {
    #[serde(default)]
    pub page_info: PageInfo,
    #[serde(default = "Vec::new")]
    pub nodes: Vec<T>,
}

impl<T> Default for Connection<T> {
    fn default() -> Self {
        Connection {
            page_info: PageInfo::default(),
            nodes: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageInfo {
    #[serde(default)]
    pub has_next_page: bool,
    #[serde(default)]
    pub end_cursor: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Label {
    pub name: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Issue {
    pub number: u64,
    pub repository: Repository,
    #[serde(default)]
    pub labels: Connection<Label>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Repository {
    pub name_with_owner: String,
}

/// What it found: the PRs that go first, and those of them it put the
/// label on (or would have, in a status run).
#[derive(Debug, Default, PartialEq, Serialize)]
pub struct Firsts {
    pub firsts: Vec<u64>,
    pub labelled: Vec<u64>,
}

/// Every page of a PR's closing issues, each with its first labels.
const CLOSING: &str = "query($owner:String!,$name:String!,$n:Int!,$cursor:String){repository(owner:$owner,name:$name){pullRequest(number:$n){closingIssuesReferences(first:100,after:$cursor){pageInfo{hasNextPage endCursor} nodes{number repository{nameWithOwner} labels(first:100){pageInfo{hasNextPage} nodes{name}}}}}}}";

/// The PRs that go first. One closing an issue marked first gets the label
/// too, unless it has it already. One GitHub won't say enough about isn't
/// first, and says why on stderr: one PR mustn't stop the rest.
pub fn firsts(p: &mut Pass, ask: &Ask) -> Firsts {
    let label = p.cfg.settle.labels.first.clone();
    let mut out = Firsts::default();
    for pr in &ask.prs {
        match goes_first(p, &label, pr) {
            Ok(None) => {}
            Ok(Some(carries)) => {
                out.firsts.push(pr.number);
                if !carries {
                    put_on(p, &label, pr.number);
                    out.labelled.push(pr.number);
                }
            }
            Err(why) => eprintln!(
                "{}#{}: whether it goes first isn't known: {why}",
                p.repo, pr.number
            ),
        }
    }
    out
}

/// Whether the PR goes first, and if it does, whether it carries the label.
fn goes_first(p: &mut Pass, label: &str, pr: &Pr) -> Result<Option<bool>, String> {
    let repo = p.repo.to_owned();
    let carries = names(p, &repo, pr.number, &pr.labels)?
        .iter()
        .any(|n| n == label);
    if carries && by_trusted(p, &repo, pr.number, label)? {
        return Ok(Some(true));
    }
    for issue in closing(p, pr)? {
        let at = issue.repository.name_with_owner.clone();
        if names(p, &at, issue.number, &issue.labels)?
            .iter()
            .any(|n| n == label)
            && by_trusted(p, &at, issue.number, label)?
        {
            return Ok(Some(carries));
        }
    }
    Ok(None)
}

/// An issue's or PR's label names: the query's, or every page of them when
/// it had more.
fn names(p: &mut Pass, repo: &str, n: u64, got: &Connection<Label>) -> Result<Vec<String>, String> {
    if !got.page_info.has_next_page {
        return Ok(got.nodes.iter().map(|l| l.name.clone()).collect());
    }
    let all = p
        .pages(&format!("repos/{repo}/issues/{n}/labels?per_page=100"))
        .map_err(|e| e.why)?;
    Ok(all
        .iter()
        .filter_map(|l| l["name"].as_str().map(String::from))
        .collect())
}

/// The issues a PR closes: the query's, or every page of them when it had
/// more.
fn closing(p: &mut Pass, pr: &Pr) -> Result<Vec<Issue>, String> {
    let Some(got) = &pr.closing_issues_references else {
        return Ok(vec![]);
    };
    if !got.page_info.has_next_page {
        return Ok(got.nodes.clone());
    }
    let (owner, name) = p
        .repo
        .split_once('/')
        .ok_or_else(|| format!("{:?} isn't OWNER/REPO", p.repo))?;
    let (owner, name) = (owner.to_owned(), name.to_owned());
    let mut all = vec![];
    let mut cursor: Option<String> = None;
    loop {
        let body = json!({"query": CLOSING, "variables": {"owner": owner, "name": name, "n": pr.number, "cursor": cursor}});
        let got: Value = call(p.api, "POST", "graphql", Some(&body), None)
            .and_then(|r| r.json())
            .map_err(|e| p.github(e))?;
        let page = &got["data"]["repository"]["pullRequest"]["closingIssuesReferences"];
        if page.is_null() {
            return Err(format!(
                "GitHub didn't list #{}'s closing issues",
                pr.number
            ));
        }
        let page: Connection<Issue> =
            serde_json::from_value(page.clone()).map_err(|e| e.to_string())?;
        all.extend(page.nodes);
        if !page.page_info.has_next_page {
            return Ok(all);
        }
        cursor = page.page_info.end_cursor;
        if cursor.is_none() {
            return Err(format!("GitHub cut #{}'s closing issues short", pr.number));
        }
    }
}

/// Whether the last time `label` went on issue or PR `n` of `repo`, an
/// account of `first_by` put it there.
fn by_trusted(p: &mut Pass, repo: &str, n: u64, label: &str) -> Result<bool, String> {
    let events = p
        .pages(&format!("repos/{repo}/issues/{n}/events?per_page=100"))
        .map_err(|e| e.why)?;
    Ok(last_by(&events, label).is_some_and(|id| p.cfg.first_by.contains(&id)))
}

/// Of an issue's events, oldest first: the account id that last put `label`
/// on.
fn last_by(events: &[Value], label: &str) -> Option<u64> {
    events
        .iter()
        .rev()
        .find(|e| e["event"] == "labeled" && e["label"]["name"].as_str() == Some(label))
        .and_then(|e| e["actor"]["id"].as_u64())
}

/// The label on PR `n`, but not in a status run. GitHub not taking it is
/// only said: the PR goes first all the same, and the next pass tries again.
fn put_on(p: &mut Pass, label: &str, n: u64) {
    if p.dry {
        eprintln!("{}#{n}: would label it {label}", p.repo);
        return;
    }
    let path = format!("repos/{}/issues/{n}/labels", p.repo);
    if let Err(e) = call(
        p.api,
        "POST",
        &path,
        Some(&json!({"labels": [label]})),
        None,
    ) {
        let why = p.github(e);
        eprintln!(
            "{}#{n}: GitHub didn't take its {label} label: {why}",
            p.repo
        );
    }
}

#[cfg(test)]
#[path = "firsts_tests.rs"]
mod tests;
