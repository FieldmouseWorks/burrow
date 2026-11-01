//! The stand-ins: a GitHub that keeps its repos, their AGENTS.md and open
//! PRs, and answers as GitHub's REST and GraphQL APIs do, and a notifier
//! that keeps what it was told.

use std::sync::{Arc, Mutex};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use burrow_github::{Api, Failed, Response};
use house_sync::pass::Notify;
use serde_json::{Value, json};

#[derive(Clone, Copy, PartialEq)]
pub enum Fails {
    No,
    /// Every call about it.
    All,
    /// Every call below the repo itself: its branches, files and PRs.
    Below,
}

#[derive(Clone)]
pub struct Repo {
    pub name: String,
    pub archived: bool,
    pub fork: bool,
    pub empty: bool,
    pub fails: Fails,
    /// A branch of the house PR's name is there already.
    pub branch_left: bool,
    pub agents: Option<String>,
    /// Files under its own house/, by name, as burrow's repo has them.
    pub source: Vec<(String, String)>,
    /// Reading its house/ gets a server error.
    pub source_fails: bool,
    /// Open PRs: number, head branch, whether that's in a fork.
    pub prs: Vec<(u64, String, bool)>,
    /// The ones it may not close.
    pub stuck: Vec<u64>,
}

pub fn repo(name: &str, agents: Option<&str>) -> Repo {
    Repo {
        name: name.into(),
        archived: false,
        fork: false,
        empty: false,
        fails: Fails::No,
        branch_left: false,
        agents: agents.map(String::from),
        source: vec![],
        source_fails: false,
        prs: vec![],
        stuck: vec![],
    }
}

pub type Asked = (String, String, Option<Value>);

pub struct GitHub {
    pub repos: Vec<Repo>,
    pub asked: Arc<Mutex<Vec<Asked>>>,
    /// Whose account each GraphQL call was about.
    pub listed: Vec<String>,
}

pub fn answer(status: u16, body: Value, link: Option<String>) -> Result<Response, Failed> {
    Ok(Response {
        status,
        headers: link.map(|l| ("Link".into(), l)).into_iter().collect(),
        body: body.to_string(),
    })
}

impl Api for GitHub {
    fn request(
        &mut self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        _: Option<&str>,
    ) -> Result<Response, Failed> {
        self.asked
            .lock()
            .unwrap()
            .push((method.into(), path.into(), body.cloned()));
        let (bare, query) = path.split_once('?').unwrap_or((path, ""));
        let parts: Vec<&str> = bare.split('/').collect();
        assert_eq!(parts[0], "repos", "{path}");
        let name = format!("{}/{}", parts[1], parts[2]);
        let Some(r) = self
            .repos
            .iter()
            .find(|r| r.name.eq_ignore_ascii_case(&name))
        else {
            return answer(404, json!({"message": "Not Found"}), None);
        };
        if r.fails == Fails::All || (r.fails == Fails::Below && parts.len() > 3) {
            return answer(500, json!({"message": "Server Error"}), None);
        }
        let name = r.name.clone();
        match (method, &parts[3..]) {
            ("GET", []) => answer(
                200,
                json!({
                    "full_name": name,
                    "default_branch": "main",
                    "owner": {"login": name.split('/').next().unwrap()},
                    "archived": r.archived,
                    "fork": r.fork,
                }),
                None,
            ),
            ("GET", ["git", "ref", "heads", "main"]) => {
                answer(200, json!({"object": {"sha": "c0ffee"}}), None)
            }
            ("POST", ["git", "refs"]) if r.branch_left => {
                answer(422, json!({"message": "Reference already exists"}), None)
            }
            ("POST", ["git", "refs"]) => answer(201, json!({}), None),
            // A branch that's gone already.
            ("DELETE", ["git", "refs", "heads", "house", "rules-gone"]) => {
                answer(422, json!({"message": "Reference does not exist"}), None)
            }
            // One it refuses to delete, with the same status.
            ("DELETE", ["git", "refs", "heads", "house", "rules-kept"]) => {
                answer(422, json!({"message": "Validation Failed"}), None)
            }
            ("PATCH" | "DELETE", ["git", "refs", "heads", ..]) => answer(200, json!({}), None),
            ("GET", ["contents", "AGENTS.md"]) => match &r.agents {
                None => answer(404, json!({"message": "Not Found"}), None),
                Some(text) => {
                    // As GitHub sends it: base64 in lines of 60.
                    let b64 = STANDARD.encode(text);
                    let lines: Vec<&str> = b64
                        .as_bytes()
                        .chunks(60)
                        .map(|c| std::str::from_utf8(c).unwrap())
                        .collect();
                    answer(
                        200,
                        json!({"sha": "f11e", "content": lines.join("\n") + "\n"}),
                        None,
                    )
                }
            },
            ("PUT", ["contents", "AGENTS.md"]) => answer(200, json!({}), None),
            ("GET", ["contents", "house", _]) if r.source_fails => {
                answer(500, json!({"message": "Server Error"}), None)
            }
            ("GET", ["contents", "house", file]) => {
                match r.source.iter().find(|(name, _)| name == file) {
                    None => answer(404, json!({"message": "Not Found"}), None),
                    Some((_, text)) => answer(200, json!({"content": STANDARD.encode(text)}), None),
                }
            }
            ("GET", ["pulls"]) => {
                assert!(query.contains("state=open"), "{path}");
                let page: usize = query
                    .split('&')
                    .find_map(|q| q.strip_prefix("page="))
                    .map_or(1, |p| p.parse().unwrap());
                let all: Vec<Value> = r
                    .prs
                    .iter()
                    .map(|(n, head, fork)| {
                        let from = if *fork { "eve/a".into() } else { name.clone() };
                        json!({
                            "number": n,
                            "html_url": format!("https://github.com/{name}/pull/{n}"),
                            "head": {"ref": head, "repo": {"full_name": from}},
                        })
                    })
                    .collect();
                let these: Vec<Value> = all
                    .iter()
                    .skip((page - 1) * 100)
                    .take(100)
                    .cloned()
                    .collect();
                let next = (all.len() > page * 100).then(|| {
                    format!(
                        "<https://api.github.com/repos/{name}/pulls?state=open&per_page=100&page={}>; rel=\"next\"",
                        page + 1
                    )
                });
                answer(200, Value::Array(these), next)
            }
            ("POST", ["pulls"]) => answer(
                201,
                json!({"html_url": format!("https://github.com/{name}/pull/9")}),
                None,
            ),
            ("POST", ["issues", _, "comments"]) => answer(201, json!({}), None),
            ("PATCH", ["pulls", n]) if r.stuck.contains(&n.parse().unwrap()) => answer(
                403,
                json!({"message": "Resource not accessible by integration"}),
                None,
            ),
            ("PATCH", ["pulls", _]) => answer(200, json!({}), None),
            _ => panic!("nothing here for {method} {path}"),
        }
    }

    /// The owner's repos that aren't forks, two a page.
    fn graphql(&mut self, owner: &str, body: &Value) -> Result<Response, Failed> {
        self.listed.push(owner.into());
        let theirs: Vec<&Repo> = self
            .repos
            .iter()
            .filter(|r| {
                r.name
                    .split('/')
                    .next()
                    .unwrap()
                    .eq_ignore_ascii_case(owner)
                    && !r.fork
            })
            .collect();
        if theirs.is_empty() {
            return answer(
                200,
                json!({"data": {"repositoryOwner": null}, "errors": [{"message": format!("Could not resolve to a RepositoryOwner with the login of '{owner}'.")}]}),
                None,
            );
        }
        let from: usize = body["variables"]["cursor"]
            .as_str()
            .map_or(0, |c| c.parse().unwrap());
        let nodes: Vec<Value> = theirs
            .iter()
            .skip(from)
            .take(2)
            .map(|r| {
                json!({
                    "nameWithOwner": r.name,
                    "isArchived": r.archived,
                    "defaultBranchRef": if r.empty { Value::Null } else { json!({"name": "main"}) },
                })
            })
            .collect();
        answer(
            200,
            json!({"data": {"repositoryOwner": {"repositories": {
                "nodes": nodes,
                "pageInfo": {"hasNextPage": theirs.len() > from + 2, "endCursor": (from + 2).to_string()},
            }}}}),
            None,
        )
    }

    fn pause(&mut self, _: u64) {}
}

#[derive(Default)]
pub struct Notices {
    pub told: Vec<Vec<String>>,
    pub fails: bool,
}

impl Notify for Notices {
    fn notify(&mut self, args: &[&str]) -> Result<(), String> {
        self.told.push(args.iter().map(|a| a.to_string()).collect());
        if self.fails {
            return Err("notify: exit status: 1".into());
        }
        Ok(())
    }
}
