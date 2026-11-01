//! What a notice asks of GitHub: its open issues, and opening, commenting on
//! and closing one.

use burrow_github::{Api, Error, call};
use serde_json::json;

/// The notice's open issues, newest first, read from the issues themselves
/// (the search index lags a new one): its label's, not PRs, titled with its
/// key in brackets. Fails when GitHub can't be asked, so that's never taken
/// for "no issue".
pub fn open_issues(
    api: &mut dyn Api,
    repo: &str,
    label: &str,
    key: &str,
) -> Result<Vec<u64>, Error> {
    let tag = format!("[{key}]");
    let mut found = Vec::new();
    let mut path = Some(format!(
        "repos/{repo}/issues?state=open&labels={}&per_page=100",
        encode(label)
    ));
    while let Some(p) = path {
        let r = call(api, "GET", &p, None, None)?;
        let page = r.json()?;
        let issues = page
            .as_array()
            .ok_or_else(|| Error(format!("GET {p}: GitHub's answer isn't a list")))?;
        for issue in issues {
            let ours = issue["pull_request"].is_null()
                && issue["title"].as_str().is_some_and(|t| t.starts_with(&tag));
            if let (true, Some(n)) = (ours, issue["number"].as_u64()) {
                found.push(n);
            }
        }
        path = r.next();
    }
    Ok(found)
}

/// The label, made if the repo hasn't got it, so a new issue doesn't come
/// out unlabelled (GitHub drops a label it doesn't know rather than fail).
/// One it can't make, most often one it already has, is no failure.
pub fn label(api: &mut dyn Api, repo: &str, label: &str, name: &str) {
    let _ = call(
        api,
        "POST",
        &format!("repos/{repo}/labels"),
        Some(
            &json!({"name": label, "color": "B60205", "description": format!("{name}'s notices")}),
        ),
        None,
    );
}

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
    call(
        api,
        "POST",
        &format!("repos/{repo}/issues"),
        Some(&issue),
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

/// A query value, percent-encoded.
fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn encodes_a_label() {
        assert_eq!(super::encode("updates"), "updates");
        assert_eq!(super::encode("needs peter&co"), "needs%20peter%26co");
        assert_eq!(super::encode("é"), "%C3%A9");
    }
}
