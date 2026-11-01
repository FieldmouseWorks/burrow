//! What a notice asks of GitHub: its open issues, and opening, commenting on
//! and closing one.

use burrow_github::{Api, Error, call, encode};
use serde_json::{Value, json};

/// The notice's open issues, newest first, read from the issues themselves
/// (the search index lags a new one): not PRs, titled with its key in
/// brackets, carrying its label or none at all. GitHub drops the labels of an
/// issue opened by a login without push access, so the label can't be what
/// finds it: one that came out bare still has to be found, or it would get a
/// twin and never close. Fails when GitHub can't be asked, so that's never
/// taken for "no issue".
pub fn open_issues(
    api: &mut dyn Api,
    repo: &str,
    label: &str,
    key: &str,
) -> Result<Vec<u64>, Error> {
    let tag = format!("[{key}]");
    let mut found = Vec::new();
    let mut path = Some(format!("repos/{repo}/issues?state=open&per_page=100"));
    while let Some(p) = path {
        let r = call(api, "GET", &p, None, None)?;
        let page = r.json()?;
        let issues = page
            .as_array()
            .ok_or_else(|| Error::said(format!("GET {p}: GitHub's answer isn't a list")))?;
        for issue in issues {
            let ours = issue["pull_request"].is_null()
                && issue["title"].as_str().is_some_and(|t| t.starts_with(&tag))
                && labelled_or_bare(issue, label);
            if let (true, Some(n)) = (ours, issue["number"].as_u64()) {
                found.push(n);
            }
        }
        path = r.next();
    }
    Ok(found)
}

fn labels(issue: &Value) -> impl Iterator<Item = &str> {
    issue["labels"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|l| l["name"].as_str())
}

/// GitHub's label names ignore case.
fn carries(issue: &Value, label: &str) -> bool {
    labels(issue).any(|l| l.eq_ignore_ascii_case(label))
}

fn labelled_or_bare(issue: &Value, label: &str) -> bool {
    labels(issue).next().is_none() || carries(issue, label)
}

/// The label, made if the repo hasn't got it. It fails when the repo may not
/// have it, so no issue is opened without its label: GitHub drops a label it
/// doesn't know rather than fail. A make that fails can still mean it's there
/// (it already was, or GitHub made it and the answer was lost), so GitHub is
/// asked which.
pub fn label(api: &mut dyn Api, repo: &str, label: &str, name: &str) -> Result<(), Error> {
    let made = call(
        api,
        "POST",
        &format!("repos/{repo}/labels"),
        Some(
            &json!({"name": label, "color": "B60205", "description": format!("{name}'s notices")}),
        ),
        None,
    );
    let Err(why) = made else { return Ok(()) };
    call(
        api,
        "GET",
        &format!("repos/{repo}/labels/{}", encode(label)),
        None,
        None,
    )
    .map(drop)
    .map_err(|_| why)
}

/// Opens the issue. GitHub drops the labels of a login without push access
/// and still answers 201, so its answer is read: a label it dropped is asked
/// for again on its own, and if that fails too the issue stands, bare, which
/// `open_issues` still finds, and the failure is the caller's to report.
pub fn create(
    api: &mut dyn Api,
    repo: &str,
    title: &str,
    body: &str,
    label: &str,
    assignee: Option<&str>,
) -> Result<(), Error> {
    let mut issue = json!({"title": title, "body": body, "labels": [label]});
    if let Some(who) = assignee {
        issue["assignees"] = json!([who]);
    }
    let made = call(
        api,
        "POST",
        &format!("repos/{repo}/issues"),
        Some(&issue),
        None,
    )?
    .json()?;
    if carries(&made, label) {
        return Ok(());
    }
    let n = made["number"].as_u64().ok_or_else(|| {
        Error::said(format!(
            "POST repos/{repo}/issues: its answer has no number"
        ))
    })?;
    call(
        api,
        "POST",
        &format!("repos/{repo}/issues/{n}/labels"),
        Some(&json!({"labels": [label]})),
        None,
    )
    .map(drop)
}

pub fn comment(api: &mut dyn Api, repo: &str, n: u64, body: &str) -> Result<(), Error> {
    call(
        api,
        "POST",
        &format!("repos/{repo}/issues/{n}/comments"),
        Some(&json!({"body": body})),
        None,
    )
    .map(drop)
}

/// Says why, then closes it, as `gh issue close --comment` does.
pub fn close(api: &mut dyn Api, repo: &str, n: u64, why: &str) -> Result<(), Error> {
    comment(api, repo, n, why)?;
    call(
        api,
        "PATCH",
        &format!("repos/{repo}/issues/{n}"),
        Some(&json!({"state": "closed"})),
        None,
    )
    .map(drop)
}
