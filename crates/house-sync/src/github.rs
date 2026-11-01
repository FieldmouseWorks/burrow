//! What a pass asks of GitHub: the repos it keeps, a default branch's
//! AGENTS.md, the open PRs, and the house PR's branch, file and PR.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use burrow_github::{Api, Error, call, call_allowing, query};
use serde_json::{Value, json};

/// A repo it keeps: its name, as GitHub spells it, and its default branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    pub name: String,
    pub base: String,
}

/// Each owner's own repos, not forks, a page at a time.
const REPOS: &str = "query($owner: String!, $cursor: String) {
  repositoryOwner(login: $owner) {
    repositories(first: 100, after: $cursor, isFork: false, ownerAffiliations: OWNER) {
      nodes { nameWithOwner isArchived defaultBranchRef { name } }
      pageInfo { hasNextPage endCursor }
    }
  }
}";

fn missing(what: &str, field: &str) -> Error {
    Error::said(format!("{what}: GitHub's answer has no {field}"))
}

/// The owner's repos it keeps: not archived, not forks, and with a default
/// branch (an empty repo has none yet).
pub fn repos(api: &mut dyn Api, owner: &str) -> Result<Vec<Repo>, Error> {
    let mut found = Vec::new();
    let mut cursor = Value::Null;
    loop {
        let data = query(api, owner, REPOS, json!({"owner": owner, "cursor": cursor}))?;
        let page = &data["repositoryOwner"]["repositories"];
        let nodes = page["nodes"]
            .as_array()
            .ok_or_else(|| missing(&format!("{owner}'s repos"), "repositories"))?;
        for node in nodes {
            if node["isArchived"] != json!(false) {
                continue;
            }
            let (Some(name), Some(base)) = (
                node["nameWithOwner"].as_str(),
                node["defaultBranchRef"]["name"].as_str(),
            ) else {
                continue;
            };
            found.push(Repo {
                name: name.into(),
                base: base.into(),
            });
        }
        if page["pageInfo"]["hasNextPage"] != json!(true) {
            return Ok(found);
        }
        cursor = page["pageInfo"]["endCursor"].clone();
    }
}

/// One repo, as a full pass would list it: None for one it doesn't keep
/// (archived, a fork, or not one of the owners').
pub fn repo(api: &mut dyn Api, name: &str, owners: &[String]) -> Result<Option<Repo>, Error> {
    let r = call(api, "GET", &format!("repos/{name}"), None, None)?.json()?;
    if r["archived"] != json!(false) || r["fork"] != json!(false) {
        return Ok(None);
    }
    let login = r["owner"]["login"].as_str().unwrap_or_default();
    if !owners.iter().any(|o| o.eq_ignore_ascii_case(login)) {
        return Ok(None);
    }
    let (Some(name), Some(base)) = (r["full_name"].as_str(), r["default_branch"].as_str()) else {
        return Err(missing(
            &format!("repos/{name}"),
            "full_name or default_branch",
        ));
    };
    Ok(Some(Repo {
        name: name.into(),
        base: base.into(),
    }))
}

/// The commit a branch points at.
pub fn head(api: &mut dyn Api, repo: &str, branch: &str) -> Result<String, Error> {
    let path = format!("repos/{repo}/git/ref/heads/{branch}");
    let r = call(api, "GET", &path, None, None)?.json()?;
    r["object"]["sha"]
        .as_str()
        .map(String::from)
        .ok_or_else(|| missing(&path, "object.sha"))
}

/// AGENTS.md at a commit: its blob's sha and its bytes, or None when there
/// isn't one.
pub fn agents(api: &mut dyn Api, repo: &str, at: &str) -> Result<Option<(String, Vec<u8>)>, Error> {
    let path = format!("repos/{repo}/contents/AGENTS.md?ref={at}");
    let r = call_allowing(api, "GET", &path, None, None, &[404])?;
    if r.status == 404 {
        return Ok(None);
    }
    let file = r.json()?;
    let (Some(sha), Some(content)) = (file["sha"].as_str(), file["content"].as_str()) else {
        return Err(missing(&path, "sha or content"));
    };
    Ok(Some((sha.into(), decode(&path, content)?)))
}

/// A file's bytes at a commit, or None when there isn't one.
pub fn file(api: &mut dyn Api, repo: &str, name: &str, at: &str) -> Result<Option<Vec<u8>>, Error> {
    let path = format!("repos/{repo}/contents/{name}?ref={at}");
    let r = call_allowing(api, "GET", &path, None, None, &[404])?;
    if r.status == 404 {
        return Ok(None);
    }
    let file = r.json()?;
    let content = file["content"]
        .as_str()
        .ok_or_else(|| missing(&path, "content"))?;
    decode(&path, content).map(Some)
}

/// The base64 a contents answer carries, in lines.
fn decode(path: &str, content: &str) -> Result<Vec<u8>, Error> {
    let content: String = content.chars().filter(|c| !c.is_whitespace()).collect();
    STANDARD
        .decode(content)
        .map_err(|e| Error::said(format!("{path}: its content isn't base64: {e}")))
}

/// An open PR.
#[derive(Debug, Clone)]
pub struct Pr {
    pub number: u64,
    pub url: String,
    /// Its head branch, and whether that's in the PR's own repo.
    pub head: String,
    pub head_here: bool,
}

pub fn open_prs(api: &mut dyn Api, repo: &str) -> Result<Vec<Pr>, Error> {
    let mut prs = Vec::new();
    let mut path = Some(format!("repos/{repo}/pulls?state=open&per_page=100"));
    while let Some(p) = path {
        let r = call(api, "GET", &p, None, None)?;
        let page = r.json()?;
        for pr in page.as_array().ok_or_else(|| missing(&p, "list of PRs"))? {
            let (Some(number), Some(head)) = (pr["number"].as_u64(), pr["head"]["ref"].as_str())
            else {
                return Err(missing(&p, "number or head.ref"));
            };
            prs.push(Pr {
                number,
                url: pr["html_url"].as_str().unwrap_or_default().into(),
                head: head.into(),
                head_here: pr["head"]["repo"]["full_name"]
                    .as_str()
                    .is_some_and(|h| h.eq_ignore_ascii_case(repo)),
            });
        }
        path = r.next();
    }
    Ok(prs)
}

/// Closes a PR with a comment that says why, and deletes its branch when
/// it's the repo's own (one already gone is fine), as `gh pr close
/// --comment --delete-branch` does.
pub fn close(api: &mut dyn Api, repo: &str, pr: &Pr, why: &str) -> Result<(), Error> {
    let n = pr.number;
    let comment = json!({ "body": why });
    call(
        api,
        "POST",
        &format!("repos/{repo}/issues/{n}/comments"),
        Some(&comment),
        None,
    )?;
    let closed = json!({ "state": "closed" });
    call(
        api,
        "PATCH",
        &format!("repos/{repo}/pulls/{n}"),
        Some(&closed),
        None,
    )?;
    if pr.head_here {
        let path = format!("repos/{repo}/git/refs/heads/{}", pr.head);
        let r = call_allowing(api, "DELETE", &path, None, None, &[422])?;
        // GitHub says a branch is gone with a 422, and says other refusals
        // with one too.
        if r.status == 422 && r.json()?["message"] != json!("Reference does not exist") {
            return Err(Error::said(format!(
                "DELETE {path}: HTTP 422: {}",
                r.body.trim()
            )));
        }
    }
    Ok(())
}

/// The house PR's title, and its commit's message.
pub const TITLE: &str = "house: the house blocks, as burrow has them now";

/// Opens the house PR: `branch` started over from `sha` (a run before this
/// one may have left it), AGENTS.md written there, and the PR. Its URL.
pub struct Open<'a> {
    pub repo: &'a str,
    pub base: &'a str,
    pub sha: &'a str,
    pub branch: &'a str,
    pub file_sha: &'a str,
    pub text: &'a [u8],
    pub rev: &'a str,
}

pub fn open(api: &mut dyn Api, o: &Open) -> Result<String, Error> {
    let repo = o.repo;
    let made = json!({ "ref": format!("refs/heads/{}", o.branch), "sha": o.sha });
    if call(
        api,
        "POST",
        &format!("repos/{repo}/git/refs"),
        Some(&made),
        None,
    )
    .is_err()
    {
        let moved = json!({ "sha": o.sha, "force": true });
        let path = format!("repos/{repo}/git/refs/heads/{}", o.branch);
        call(api, "PATCH", &path, Some(&moved), None)?;
    }
    let file = json!({
        "branch": o.branch,
        "sha": o.file_sha,
        "message": TITLE,
        "content": STANDARD.encode(o.text),
    });
    call(
        api,
        "PUT",
        &format!("repos/{repo}/contents/AGENTS.md"),
        Some(&file),
        None,
    )?;
    let pr = json!({
        "title": TITLE,
        "head": o.branch,
        "base": o.base,
        "body": format!(
            "burrow's house/ changed (now {}). This rewrites only what's between the house markers in AGENTS.md, so each block matches. Change the blocks in FieldmouseWorks/burrow, not here.",
            o.rev
        ),
    });
    let path = format!("repos/{repo}/pulls");
    let made = call(api, "POST", &path, Some(&pr), None)?.json()?;
    made["html_url"]
        .as_str()
        .map(String::from)
        .ok_or_else(|| missing(&path, "html_url"))
}
