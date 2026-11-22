//! The scripted GitHub: one PR that Conary's reads and burrow's writes
//! both reach, so what the producer posts is what it reads back.
use super::{FLOW, HEAD, PR, REPO, TREE};
use crate::Read;
use crate::discussion::Discussion;
use crate::producer::GitHub;
use burrow_github::fake::ok;
use burrow_github::{Api, Failed, Response as GhResponse};
use conary_github::api::{
    ApiError, CheckRun, Comment, Commit, Event, History, PullRequest, Pulls, Review, Status,
};
use conary_github::branches::{Branches, Merged};
use conary_github::flow::{self, ReviewThread, Revision, Snapshot, ThreadComment, fingerprint};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

/// The PR as the scripted GitHub has it, and what was posted to it.
pub struct Pr {
    pub head: String,
    pub base: String,
    /// The base branch's commit.
    pub base_sha: String,
    pub title: String,
    pub body: String,
    pub threads: BTreeMap<String, ReviewThread>,
    pub reviews: Vec<Value>,
    pub comments: Vec<Value>,
    /// Each check run started (its name), and each completed (its body).
    pub runs: Vec<Value>,
    /// How many comment posts GitHub refuses before it takes one.
    pub refusing: u32,
    /// The comments on another PR on the same head, by its number.
    pub others: BTreeMap<u64, Vec<Value>>,
    /// What CI reports on the head: its check runs and its statuses, as
    /// GitHub's REST API has them.
    pub check_runs: Vec<Value>,
    pub statuses: Vec<Value>,
}

#[derive(Clone)]
pub struct World(Arc<Mutex<Pr>>);

pub fn world() -> World {
    World(Arc::new(Mutex::new(Pr {
        head: HEAD.into(),
        base: "main".into(),
        base_sha: "3333333333333333333333333333333333333333".into(),
        title: "Fix it".into(),
        body: "It fixes it.".into(),
        threads: BTreeMap::from([(
            "T1".into(),
            ReviewThread {
                resolved: true,
                path: "src/lib.rs".into(),
                line: Some(1),
                comments: vec![
                    ThreadComment {
                        author: Some("reviewer".into()),
                        body: "P2: the loop never ends".into(),
                    },
                    ThreadComment {
                        author: Some(FLOW.into()),
                        body: "It ends on the empty line, by design.".into(),
                    },
                ],
            },
        )]),
        reviews: vec![
            json!({"id": 9, "state": "COMMENTED", "body": "One finding.",
            "user": {"login": "reviewer", "type": "Bot"}}),
        ],
        comments: vec![],
        check_runs: vec![],
        statuses: vec![],
        runs: vec![],
        refusing: 0,
        others: BTreeMap::new(),
    })))
}

impl World {
    pub fn with<T>(&self, f: impl FnOnce(&mut Pr) -> T) -> T {
        f(&mut self.0.lock().unwrap())
    }
    pub fn against(&self) -> String {
        self.with(|p| format!("{}\n{}", p.title, p.body))
    }
    pub fn revision(&self) -> Revision {
        self.with(|p| Revision {
            head: p.head.clone(),
            base: p.base.clone(),
            description: fingerprint(&format!("{}\n{}", p.title, p.body)),
        })
    }
    pub fn context(&self) -> String {
        Discussion::read(self, REPO, PR).unwrap().context()
    }
    pub fn posted(&self) -> Vec<String> {
        self.with(|p| {
            p.comments
                .iter()
                .map(|c| c["body"].as_str().unwrap().to_owned())
                .collect()
        })
    }
    /// The check runs completed, by conclusion, and their output texts.
    pub fn completed(&self) -> Vec<(String, String)> {
        self.with(|p| {
            p.runs
                .iter()
                .filter(|r| r["status"] == "completed")
                .map(|r| {
                    (
                        r["conclusion"].as_str().unwrap().to_owned(),
                        r["output"]["text"].as_str().unwrap_or("").to_owned(),
                    )
                })
                .collect()
        })
    }
}

impl Api for World {
    fn request(
        &mut self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        _: Option<&str>,
    ) -> Result<GhResponse, Failed> {
        let mut pr = self.0.lock().unwrap();
        let rest = path.strip_prefix(&format!("repos/{REPO}")).unwrap_or(path);
        match (method, rest) {
            ("POST", "/issues/5/comments") if pr.refusing > 0 => {
                pr.refusing -= 1;
                ok(502, json!({}), &[])
            }
            ("POST", "/issues/5/comments") => {
                pr.comments
                    .push(json!({"user": {"login": FLOW, "type": "Bot"},
                    "body": body.unwrap()["body"]}));
                ok(201, json!({}), &[])
            }
            ("GET", p) if p.starts_with("/issues/5/comments") => ok(200, json!(pr.comments), &[]),
            ("POST", p) if other(p).is_some() => {
                let comment = json!({"user": {"login": FLOW, "type": "Bot"},
                    "body": body.unwrap()["body"]});
                pr.others
                    .entry(other(p).unwrap())
                    .or_default()
                    .push(comment);
                ok(201, json!({}), &[])
            }
            ("GET", p) if other(p).is_some() => {
                let n = other(p).unwrap();
                ok(
                    200,
                    json!(pr.others.get(&n).cloned().unwrap_or_default()),
                    &[],
                )
            }
            ("GET", p) if p.starts_with("/commits/") && p.contains("/check-runs") => {
                ok(200, json!({"total_count": 0, "check_runs": []}), &[])
            }
            ("POST", "/check-runs") => {
                pr.runs.push(json!({"start": body.unwrap()["name"]}));
                ok(201, json!({"id": pr.runs.len()}), &[])
            }
            ("PATCH", p) if p.starts_with("/check-runs/") => {
                pr.runs.push(body.unwrap().clone());
                ok(200, json!({}), &[])
            }
            _ => panic!("nothing scripted for {method} {path}"),
        }
    }
}

/// The number of another PR than `PR` a comments path is for.
fn other(path: &str) -> Option<u64> {
    let n: u64 = path
        .strip_prefix("/issues/")?
        .split_once("/comments")?
        .0
        .parse()
        .ok()?;
    (n != PR).then_some(n)
}

impl conary_github::Source for World {
    fn pull(&self, _: &str, n: u64) -> Result<PullRequest, ApiError> {
        let p = self.0.lock().unwrap();
        Ok(serde_json::from_value(json!({
            "number": n, "state": "open", "title": p.title, "body": p.body,
            "user": {"login": "someone", "type": "User"},
            "head": {"sha": p.head, "ref": "topic", "repo": null},
            "base": {"sha": p.base_sha, "ref": p.base, "repo": null},
        }))
        .unwrap())
    }
    fn comments(&self, _: &str, n: u64) -> Result<Vec<Comment>, ApiError> {
        let p = self.0.lock().unwrap();
        let comments = if n == PR {
            p.comments.clone()
        } else {
            p.others.get(&n).cloned().unwrap_or_default()
        };
        Ok(serde_json::from_value(json!(comments)).unwrap())
    }
    fn reviews(&self, _: &str, _: u64) -> Result<Vec<Review>, ApiError> {
        Ok(serde_json::from_value(json!(self.0.lock().unwrap().reviews)).unwrap())
    }
    fn diff(&self, _: &str, _: &str, _: &str) -> Result<String, ApiError> {
        unreachable!()
    }
    fn compare_commits(&self, _: &str, _: &str, _: &str) -> Result<History, ApiError> {
        unreachable!()
    }
    fn pulls(&self, _: &str, _: Pulls) -> Result<Vec<PullRequest>, ApiError> {
        unreachable!()
    }
    fn check_runs(&self, _: &str, _: &str) -> Result<Vec<CheckRun>, ApiError> {
        let runs = self.with(|p| p.check_runs.clone());
        Ok(serde_json::from_value(Value::Array(runs)).unwrap())
    }
    fn statuses(&self, _: &str, _: &str) -> Result<Vec<Status>, ApiError> {
        let statuses = self.with(|p| p.statuses.clone());
        Ok(serde_json::from_value(Value::Array(statuses)).unwrap())
    }
    fn commit(&self, _: &str, _: &str) -> Result<Commit, ApiError> {
        unreachable!()
    }
    fn commit_pulls(&self, _: &str, _: &str) -> Result<Vec<u64>, ApiError> {
        unreachable!()
    }
    fn timeline(&self, _: &str, _: u64) -> Result<Vec<Event>, ApiError> {
        unreachable!()
    }
    fn merge_base(&self, _: &str, _: &str, _: &str) -> Result<String, ApiError> {
        unreachable!()
    }
    fn files(&self, _: &str, _: &str, _: &str) -> Result<conary_core::policy::Paths, ApiError> {
        unreachable!()
    }
    fn annotations(&self, _: &str, _: u64) -> Result<Vec<String>, ApiError> {
        unreachable!()
    }
}

impl Branches for World {
    fn tree(&self, _: &str, _: &str) -> Result<String, ApiError> {
        Ok(TREE.into())
    }
    fn default_branch(&self, _: &str) -> Result<String, ApiError> {
        unreachable!()
    }
    fn branch(&self, _: &str, _: &str) -> Result<Option<String>, ApiError> {
        unreachable!()
    }
    fn branches(&self, _: &str, _: &str) -> Result<Vec<String>, ApiError> {
        unreachable!()
    }
    fn create_branch(&self, _: &str, _: &str, _: &str) -> Result<(), ApiError> {
        unreachable!()
    }
    fn delete_branch(&self, _: &str, _: &str) -> Result<(), ApiError> {
        unreachable!()
    }
    fn merge(&self, _: &str, _: &str, _: &str, _: &str) -> Result<Merged, ApiError> {
        unreachable!()
    }
}

impl flow::Read for World {
    fn snapshot(&self, _: &str, _: u64) -> Result<Snapshot, ApiError> {
        Ok(Snapshot {
            revision: self.revision(),
            default_branch: None,
            draft: false,
            complete: true,
            merge_state: "CLEAN".into(),
            open_threads: 0,
            threads: self.0.lock().unwrap().threads.clone(),
            resolved_reviews: BTreeSet::new(),
            updated_at: 0,
            suites: BTreeMap::new(),
            checks_complete: true,
            rebase_removed: vec![],
            timeline_cut_at: None,
            since: Default::default(),
        })
    }
    fn pushes(&self, _: &str, _: &str) -> Result<BTreeMap<String, String>, ApiError> {
        unreachable!()
    }
}

pub fn read(w: &World) -> Read {
    let w = w.clone();
    Box::new(move || Ok(Box::new(w.clone()) as Box<dyn flow::Read>))
}

pub fn github(w: &World) -> GitHub {
    let w = w.clone();
    Box::new(move || Box::new(w.clone()))
}
