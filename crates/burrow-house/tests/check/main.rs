//! The check, run against a stand-in for GitHub: a PR whose title, body and
//! head it reads each look, the comparison of two commits, files' content
//! at a commit, and the statuses it posts. A PR that doesn't break a rule
//! passes it whatever its body says; one that does fails it unless its body
//! has the rule's line with a reason. Each verdict is a status for the PR on
//! the head the event names, posted again whenever the PR changed while it
//! was judged; a fork's PR, which can't post them, gets them as the run's
//! result.

mod file_size;
mod model_names;
mod runs;
mod rust_first;
mod timers;
mod title;

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};

use burrow_house::check::{self, Event};
use burrow_house::github::{GitHub, Status};
use serde_json::{Value, json};

/// The defaults a caller gets.
pub const GROWTH: i64 = 50;
pub const LINES: usize = 1000;

/// A file in a comparison, as GitHub lists it.
pub fn change(status: &str, filename: &str, added: u64, removed: u64) -> Value {
    json!({"status": status, "filename": filename, "additions": added, "deletions": removed})
}

/// One renamed or copied from another path.
pub fn from(mut file: Value, previous: &str) -> Value {
    file["previous_filename"] = json!(previous);
    file
}

/// One with its patch.
pub fn patched(mut file: Value, patch: String) -> Value {
    file["patch"] = json!(patch);
    file
}

/// A file's patch as GitHub gives it: a hunk with these lines added, others
/// kept and taken out around them.
pub fn adds(lines: &[&str], context: &[&str], removes: &[&str]) -> String {
    let mut patch = vec!["@@ -1,3 +1,4 @@".to_owned()];
    patch.extend(context.iter().map(|l| format!(" {l}")));
    patch.extend(removes.iter().map(|l| format!("-{l}")));
    patch.extend(lines.iter().map(|l| format!("+{l}")));
    patch.join("\n")
}

/// What happens before the run's Nth post: the PR changes, and other runs
/// post theirs.
#[derive(Clone, Default)]
pub struct Change {
    pub body: Option<&'static str>,
    pub title: Option<&'static str>,
    pub head: Option<&'static str>,
    /// Its sha and ref.
    pub base: Option<(&'static str, &'static str)>,
    /// Statuses other runs post meanwhile for rust-first, as (sha, state).
    pub others: Vec<(&'static str, &'static str)>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Fork {
    No,
    Yes,
    /// The fork is gone: its head repo is null.
    Gone,
}

/// A status as posted, by this run or another.
#[derive(Clone, Debug)]
pub struct Posted {
    pub sha: String,
    pub status: Status,
    pub by: &'static str,
}

/// GitHub, answered from a case, logging every call.
struct Fake {
    pr: RefCell<Value>,
    /// "BASE...HEAD": its comparison.
    compares: HashMap<String, Value>,
    /// (sha, path): its content; one missing gets no answer.
    trees: HashMap<(String, String), String>,
    changes: BTreeMap<usize, Change>,
    /// The PR's body changes every time it's read.
    restless: bool,
    /// GitHub answers nothing.
    down: bool,
    calls: RefCell<Vec<String>>,
    posted: RefCell<Vec<Posted>>,
    posts: Cell<usize>,
}

impl Fake {
    fn call(&self, call: String) -> Result<(), String> {
        self.calls.borrow_mut().push(call);
        if self.down {
            return Err("GitHub didn't answer".into());
        }
        Ok(())
    }
}

impl GitHub for Fake {
    fn pull(&self, number: u64) -> Result<Value, String> {
        self.call(format!("pull {number}"))?;
        assert_eq!(number, 7);
        if self.restless {
            let body = format!("{}!", self.pr.borrow()["body"].as_str().unwrap());
            self.pr.borrow_mut()["body"] = json!(body);
        }
        Ok(self.pr.borrow().clone())
    }

    fn compare(&self, base: &str, head: &str) -> Result<Value, String> {
        self.call(format!("compare {base}...{head}"))?;
        let key = format!("{base}...{head}");
        self.compares
            .get(&key)
            .cloned()
            .ok_or(format!("no comparison {key}"))
    }

    fn content(&self, path: &str, at: &str) -> Result<Vec<u8>, String> {
        self.call(format!("content {path:?}@{at}"))?;
        let key = (at.to_owned(), path.to_owned());
        let content = self.trees.get(&key).ok_or(format!("no {path:?} at {at}"))?;
        Ok(content.as_bytes().to_vec())
    }

    fn post(&self, sha: &str, status: &Status) -> Result<(), String> {
        self.call(format!("post {sha}"))?;
        let n = self.posts.get() + 1;
        self.posts.set(n);
        if let Some(c) = self.changes.get(&n) {
            let mut pr = self.pr.borrow_mut();
            if let Some(body) = c.body {
                pr["body"] = json!(body);
            }
            if let Some(title) = c.title {
                pr["title"] = json!(title);
            }
            if let Some(head) = c.head {
                pr["head"]["sha"] = json!(head);
            }
            if let Some((sha, name)) = c.base {
                pr["base"] = json!({"sha": sha, "ref": name});
            }
            for (sha, state) in &c.others {
                self.posted.borrow_mut().push(Posted {
                    sha: (*sha).to_owned(),
                    status: Status {
                        context: "house/rust-first (#7)".into(),
                        state: if *state == "success" {
                            "success"
                        } else {
                            "failure"
                        },
                        description: String::new(),
                        target_url: String::new(),
                    },
                    by: "another run",
                });
            }
        }
        self.posted.borrow_mut().push(Posted {
            sha: sha.to_owned(),
            status: status.clone(),
            by: "this run",
        });
        Ok(())
    }
}

/// A run's case: an event naming head 4ead, on a PR whose head is `pr_head`
/// with these files (the comparison of base ba5e and 4ead, merge base 3e7b),
/// title and body.
#[derive(Clone)]
pub struct Case {
    pub files: Vec<Value>,
    pub body: String,
    pub title: String,
    /// Files' content at a commit, (sha, path, content); None, GitHub
    /// doesn't answer for it. A file with an extension that a comparison
    /// leaves in place is as long as the lines it adds, unless this says
    /// otherwise.
    pub trees: Vec<(&'static str, String, Option<String>)>,
    pub growth: i64,
    pub lines: usize,
    pub model_names: bool,
    pub fork: Fork,
    pub changes: BTreeMap<usize, Change>,
    pub restless: bool,
    /// The files of other comparisons, (base, head, files).
    pub compares: Vec<(&'static str, &'static str, Vec<Value>)>,
    pub pr_head: &'static str,
    pub down: bool,
}

pub const TITLE: &str = "tools: a plain sentence";

pub fn case(files: Vec<Value>) -> Case {
    Case {
        files,
        body: String::new(),
        title: TITLE.into(),
        trees: vec![],
        growth: GROWTH,
        lines: LINES,
        model_names: true,
        fork: Fork::No,
        changes: BTreeMap::new(),
        restless: false,
        compares: vec![],
        pr_head: "4ead",
        down: false,
    }
}

impl Case {
    pub fn body(mut self, body: &str) -> Self {
        self.body = body.into();
        self
    }

    pub fn title(mut self, title: &str) -> Self {
        self.title = title.into();
        self
    }

    pub fn tree(mut self, sha: &'static str, path: &str, content: &str) -> Self {
        self.trees.push((sha, path.into(), Some(content.into())));
        self
    }

    /// GitHub gives no answer for a path at a commit.
    pub fn unread(mut self, sha: &'static str, path: &str) -> Self {
        self.trees.push((sha, path.into(), None));
        self
    }

    pub fn change(mut self, n: usize, change: Change) -> Self {
        self.changes.insert(n, change);
        self
    }

    pub fn compare(mut self, base: &'static str, head: &'static str, files: Vec<Value>) -> Self {
        self.compares.push((base, head, files));
        self
    }

    pub fn run(&self) -> Outcome {
        let head_repo = match self.fork {
            Fork::No => json!({"full_name": "sam/r"}),
            Fork::Yes => json!({"full_name": "fork/r"}),
            Fork::Gone => Value::Null,
        };
        let pr = json!({
            "title": self.title, "body": self.body,
            "head": {"sha": self.pr_head, "repo": head_repo},
            "base": {"sha": "ba5e", "ref": "main"},
        });
        let mut comparisons = vec![("ba5e", "4ead", self.files.clone())];
        comparisons.extend(self.compares.iter().cloned());
        let mut trees = HashMap::new();
        for (_, head, files) in &comparisons {
            for f in files {
                let name = f["filename"].as_str().unwrap();
                if f["status"] != "removed" && name.rsplit('/').next().unwrap().contains('.') {
                    let lines = "x\n".repeat(f["additions"].as_u64().unwrap() as usize);
                    trees.insert((head.to_string(), name.to_owned()), lines);
                }
            }
        }
        for (sha, path, content) in &self.trees {
            let key = (sha.to_string(), path.clone());
            match content {
                Some(c) => trees.insert(key, c.clone()),
                None => trees.remove(&key),
            };
        }
        let gh = Fake {
            pr: RefCell::new(pr),
            compares: comparisons
                .into_iter()
                .map(|(b, h, files)| {
                    let compare = json!({"merge_base_commit": {"sha": "3e7b"}, "files": files});
                    (format!("{b}...{h}"), compare)
                })
                .collect(),
            trees,
            changes: self.changes.clone(),
            restless: self.restless,
            down: self.down,
            calls: RefCell::default(),
            posted: RefCell::default(),
            posts: Cell::new(0),
        };
        let event = Event {
            repo: "sam/r".into(),
            number: 7,
            head: "4ead".into(),
            growth: self.growth,
            lines: self.lines,
            model_names: self.model_names,
            run_url: "https://github.example/sam/r/actions/runs/99".into(),
        };
        let mut out = Vec::new();
        let result = check::run(&event, &gh, &mut out);
        Outcome {
            result,
            out: String::from_utf8(out).unwrap(),
            calls: gh.calls.into_inner(),
            posted: gh.posted.into_inner(),
        }
    }

    fn rules(&self) -> &'static [&'static str] {
        if self.model_names {
            &RULES
        } else {
            &RULES[..4]
        }
    }
}

pub struct Outcome {
    pub result: Result<bool, String>,
    pub out: String,
    pub calls: Vec<String>,
    pub posted: Vec<Posted>,
}

impl Outcome {
    pub fn errors(&self) -> Vec<&str> {
        self.out
            .lines()
            .filter(|l| l.starts_with("::error"))
            .collect()
    }
}

pub const RULES: [&str; 5] = ["title", "rust-first", "timers", "file-size", "model-names"];

/// What each rule's error is titled, and the line its body wants.
pub fn wants(rule: &str) -> (&'static str, &'static str) {
    match rule {
        "title" => ("PR title", "area: plain sentence"),
        "rust-first" => ("Why not Rust?", "Why not Rust:"),
        "timers" => ("Why a timer?", "Why a timer:"),
        "file-size" => ("Why not split?", "Why not split:"),
        "model-names" => ("Why a model name?", "Why a model name:"),
        _ => unreachable!("{rule}"),
    }
}

/// The statuses one look posted: one per rule, in order, `failure` for the
/// failing ones and `success` for the rest, each for this PR's head, linking
/// the run.
pub fn status(posted: &[Posted], failing: &[&str], rules: &[&str]) {
    let contexts: Vec<String> = posted.iter().map(|p| p.status.context.clone()).collect();
    let expected: Vec<String> = rules.iter().map(|r| format!("house/{r} (#7)")).collect();
    assert_eq!(contexts, expected, "{posted:#?}");
    for (rule, p) in rules.iter().zip(posted) {
        let state = if failing.contains(rule) {
            "failure"
        } else {
            "success"
        };
        assert_eq!(
            (p.status.state, p.sha.as_str()),
            (state, "4ead"),
            "{rule}: {p:#?}"
        );
        assert_eq!(
            p.status.target_url,
            "https://github.example/sam/r/actions/runs/99"
        );
        let length = p.status.description.chars().count();
        assert!(0 < length && length <= 140, "{p:#?}");
    }
}

/// It fails the rule: the run passes, posting a failed status for it and
/// passing every other, or for a fork's PR fails, posting none. The error
/// names what it wants. It's returned.
#[track_caller]
pub fn fails(c: &Case, rule: &str) -> String {
    let o = c.run();
    let (title, wants) = wants(rule);
    let errors = o.errors();
    assert!(
        errors.len() == 1
            && errors[0].starts_with(&format!("::error title={title}::"))
            && errors[0].contains(wants),
        "{rule}: {:?}\n{}",
        o.result,
        o.out
    );
    if c.fork == Fork::No {
        assert_eq!(o.result, Ok(true), "{}", o.out);
        status(&o.posted, &[rule], c.rules());
    } else {
        assert_eq!(o.result, Ok(false), "{}", o.out);
        assert!(o.posted.is_empty(), "{:#?}", o.posted);
    }
    errors[0].to_owned()
}

#[track_caller]
pub fn passes(c: &Case) -> Outcome {
    let o = c.run();
    assert!(
        o.result == Ok(true) && !o.out.contains("::error"),
        "{:?}\n{}",
        o.result,
        o.out
    );
    if c.fork == Fork::No {
        status(&o.posted, &[], c.rules());
    } else {
        assert!(o.posted.is_empty(), "{:#?}", o.posted);
    }
    o
}

/// What was posted for one rule, in order: (sha, state, by).
pub fn said(posted: &[Posted], rule: &str) -> Vec<(String, &'static str, &'static str)> {
    posted
        .iter()
        .filter(|p| p.status.context == format!("house/{rule} (#7)"))
        .map(|p| (p.sha.clone(), p.status.state, p.by))
        .collect()
}

pub const REASON: &str = "Why not Rust: a throwaway script for one migration.";

/// A code file one line past the limit.
pub fn long() -> String {
    "x\n".repeat(LINES + 1)
}
