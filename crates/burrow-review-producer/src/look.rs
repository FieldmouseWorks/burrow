//! A look: what the producer read and what it said, kept for the sign pass
//! (`looks/ID.json`), and how one runs: in the reviewers' service, queued
//! as a fresh look on a snapshot of its own (burrow-review's `fresh`).
use burrow_producer::serve::Work;
use burrow_review::queue::{self, Item};
use conary_core::plugin::{Finding, Severity};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A look the producer answered the flow with: everything its publication
/// is held to before it's signed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Look {
    pub id: String,
    pub repo: String,
    pub pr: u64,
    pub role: String,
    pub head: String,
    /// The head's tree, which the signature names.
    pub tree: String,
    /// The base's tip the request named, and its branch, as the review's
    /// marker names it (`base`).
    pub base: String,
    pub base_ref: String,
    /// The PR's description as the look read it: `flow::fingerprint` of
    /// its title and body, as the marker's `scope` names it.
    pub scope: String,
    /// SHA-256 of the earlier findings and replies the brief quoted.
    pub prior: String,
    /// When it read them: what was said later isn't in `prior`.
    pub asked: u64,
    pub findings: Vec<Finding>,
    /// When the look's answer came back: no publication is older.
    pub finished: u64,
    /// The publication it was signed for, by its issuance, once it was.
    #[serde(default)]
    pub signed: Option<String>,
}

/// The looks kept in `dir`.
pub struct Looks(pub PathBuf);

impl Looks {
    pub fn open(dir: &Path) -> Result<Self, String> {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        Ok(Self(dir.to_path_buf()))
    }

    fn path(&self, id: &str) -> PathBuf {
        self.0.join(format!("{id}.json"))
    }

    /// Keep `look`, whole or not at all.
    pub fn keep(&self, look: &Look) -> Result<(), String> {
        let path = self.path(&look.id);
        let tmp = path.with_extension("tmp");
        let text = serde_json::to_vec_pretty(look).map_err(|e| e.to_string())?;
        fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
        fs::rename(&tmp, &path).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Let go of look `id`.
    pub fn drop(&self, id: &str) -> Result<(), String> {
        match fs::remove_file(self.path(id)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(()),
        }
    }

    /// Every look kept, by id; one that doesn't read is let go.
    pub fn all(&self) -> Result<Vec<Look>, String> {
        let mut looks = vec![];
        for entry in fs::read_dir(&self.0).map_err(|e| format!("{}: {e}", self.0.display()))? {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.extension().is_none_or(|x| x != "json") {
                continue;
            }
            match fs::read(&path)
                .ok()
                .and_then(|t| serde_json::from_slice(&t).ok())
            {
                Some(look) => looks.push(look),
                None => {
                    eprintln!("{} doesn't read as a look; let go", path.display());
                    let _ = fs::remove_file(&path);
                }
            }
        }
        looks.sort_by(|a: &Look, b: &Look| a.finished.cmp(&b.finished).then(a.id.cmp(&b.id)));
        Ok(looks)
    }
}

/// What a look said, and what ran it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ran {
    /// The reviewer's answer, as its brief asks for it.
    pub answer: Value,
    pub session: String,
    pub model: Option<String>,
    pub effort: Option<String>,
}

/// Where a look runs.
pub trait Looker: Send + Sync {
    /// Run `item`, a fresh look, to its end: what it said, or why it said
    /// nothing. Abandoned work stops it, and it says nothing.
    fn look(&self, item: &Item, work: &Work) -> Result<Ran, String>;
}

/// The reviewers' service's queue: a look is an entry in it, and its
/// answer the file the service leaves under the same name.
pub struct Queue {
    pub queue: PathBuf,
    pub done: PathBuf,
    /// How often the answer is looked for.
    pub every: Duration,
    pub reviewer_uid: u32,
    /// Unit fixtures can establish a private directory boundary even when
    /// their build ancestors are writable. No production configuration or
    /// executable can select this boundary.
    #[cfg(test)]
    pub(crate) answer_root: Option<fs::File>,
}

impl Queue {
    fn read_answer(&self, path: &Path, request: &[u8]) -> std::io::Result<Value> {
        #[cfg(test)]
        if let Some(root) = &self.answer_root {
            return burrow_review::answer::read_from(
                root.try_clone()?,
                Path::new(path.file_name().expect("answer name")),
                self.reviewer_uid,
                request,
            );
        }
        burrow_review::answer::read(path, self.reviewer_uid, request)
    }
}

impl Looker for Queue {
    fn look(&self, item: &Item, work: &Work) -> Result<Ran, String> {
        let name = format!("{}--{}.json", item.reviewer, item.snap);
        let entry = self.queue.join(&name);
        let back = self.done.join(&name);
        let value = serde_json::to_value(item).map_err(|e| e.to_string())?;
        let request = serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?;
        queue::save(&entry, &value).map_err(|e| format!("{}: {e}", entry.display()))?;
        // Its entry going is how the service learns the look is to stop.
        let answer = loop {
            if work.abandoned() {
                let _ = queue::remove(&entry);

                return Err("its request was abandoned: the look is stopped".into());
            }
            match self.read_answer(&back, &request) {
                Ok(answer) => break answer,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    let _ = queue::remove(&entry);
                    return Err(format!("untrusted review answer: {e}"));
                }
            }
            std::thread::sleep(self.every);
        };

        let _ = queue::remove(&entry);
        ran(&answer)
    }
}

/// What the service left: a review and what ran it, or why there's none.
fn ran(back: &Value) -> Result<Ran, String> {
    let text = |v: &Value| v.as_str().map(str::to_owned);
    match back["result"].as_str() {
        Some("review") => {
            let ran = &back["ran"];
            let session = text(&ran["session"])
                .filter(|s| !s.trim().is_empty())
                .ok_or("the look names no session")?;
            Ok(Ran {
                answer: back["answer"].clone(),
                session,
                model: text(&ran["model"]),
                effort: text(&ran["effort"]),
            })
        }
        Some("out") => Err(format!(
            "the reviewer is out: {}",
            back["why"].as_str().unwrap_or("it didn't say why")
        )),
        _ => Err(format!(
            "the look failed: {}",
            back["error"].as_str().unwrap_or("it didn't say why")
        )),
    }
}

/// The look's findings, as Conary's contract has them: each one's place,
/// its severity lowercased, and its title, body and what a fix covers as
/// its text. A finding that isn't one fails the whole answer.
pub fn findings(answer: &Value) -> Result<Vec<Finding>, String> {
    let empty = vec![];
    let listed = match &answer["findings"] {
        Value::Null => &empty,
        Value::Array(list) => list,
        _ => return Err("the look's findings aren't a list".into()),
    };
    listed
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let text = |k: &str| f[k].as_str().unwrap_or_default().trim().to_owned();
            let severity = match text("severity").to_ascii_uppercase().as_str() {
                "P0" => Severity::P0,
                "P1" => Severity::P1,
                "P2" => Severity::P2,
                other => return Err(format!("finding {i}'s severity {other:?} isn't one")),
            };
            let mut said = format!("{}\n\n{}", text("title"), text("body"));
            if !text("covers").is_empty() {
                said += &format!("\n\nCovers: {}", text("covers"));
            }
            let finding = Finding {
                path: text("path"),
                line: f["line"].as_u64().unwrap_or(0),
                severity,
                text: said.trim().to_owned(),
            };
            if finding.valid() {
                Ok(finding)
            } else {
                Err(format!(
                    "finding {i} doesn't name a line of a file with its reason"
                ))
            }
        })
        .collect()
}

#[cfg(test)]
#[path = "look_tests.rs"]
mod tests;
