//! burrow-review-ask: the one door through which the pipeline acts as the
//! owner. Codex's GitHub connector takes a review ask only from the owner of
//! the ChatGPT account it's set up on: an App's `@codex review` gets no
//! review, and an ask with any text after it is read as a task. So when the
//! steward finds a head the connector hasn't reviewed (review_step.py), or
//! Conary's flow does in a repo conaryd lands (its `review_ask`), it runs
//! the door's command, which asks the door's broker (broker.rs,
//! modules/autoland/review-ask.nix), a service of the machine user's, to run
//! this, and this posts the bare ask with the user's own gh login, the one
//! a person's issue's job acts as. The units that hold the App's key, and
//! the flow's controller, never hold that login.
//!
//! It posts [`ASK`] and nothing else, ever, and only when every check holds,
//! read as the owner sees the PR right then:
//!
//! - the repo is one the machine's flake lists (`burrow.autoland.askAsOwner.repos`);
//! - the PR is open, not a draft, and its head is the one the steward named;
//! - it carries none of the labels that hold it (`hold`, `waiting: release age`);
//! - the connector has said nothing of the head yet, or found something and
//!   every thread of it is resolved (answered without a push), and it has
//!   no ask in hand (its 👀 on the PR or a comment);
//! - it hasn't been asked on this head with Codex's word as it is now
//!   (once a head, as the record below keeps it);
//! - fewer than the day's cap of asks went in the last 24 hours.
//!
//! The record (`asked.json` in its state dir) is the one home of what was
//! asked: each ask's repo, PR, head, `said` (when Codex last said anything
//! of that head, in seconds, 0 for nothing) and `at`. The steward reads it
//! to know whether, and when, a head was asked, and starts this only when it
//! wasn't. An ask is recorded before it's posted, so no crash or retry can
//! post it twice; GitHub's plain no takes the record back.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use burrow_github::{Api, call, encode, query};
use burrow_upkeep::flaky::login;
use burrow_upkeep::verdict::{self, CONNECTOR, Verdict};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The whole of what it posts: the connector's documented trigger, bare.
pub const ASK: &str = "@codex review";

/// How far back the day's cap looks, in seconds.
const DAY: u64 = 24 * 60 * 60;
/// How long an ask stays in the record, in seconds: past it, an open PR
/// whose head still hasn't moved may be asked again.
const KEEP: u64 = 30 * DAY;

/// What the machine's flake gives it (modules/autoland.nix).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Config {
    /// The repos (OWNER/NAME) it may ask in.
    pub repos: Vec<String>,
    /// The most asks in any 24 hours.
    pub per_day: usize,
    /// Where its record is kept.
    pub state_dir: PathBuf,
    /// The labels that hold a PR: none is asked for while it has one.
    pub held: Vec<String>,
}

/// One ask, as the record keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Asked {
    pub repo: String,
    pub pr: u64,
    pub head: String,
    /// When Codex last said anything of the head as the ask went, in
    /// seconds (0: nothing yet).
    pub said: u64,
    pub at: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    asked: Vec<Asked>,
}

/// What came of it, as it prints it for the steward.
#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "lowercase")]
pub enum Outcome {
    /// It posted the ask now.
    Posted { at: u64 },
    /// This head was asked already, with Codex's word as it is, at `at`.
    Asked { at: u64 },
    /// A check said no, and nothing was posted.
    Declined { why: String },
    /// GitHub couldn't be read, or refused the comment.
    Failed { why: String },
}

impl Outcome {
    /// Its exit: 0 only once the head is asked (posted now, or before), as
    /// Conary's flow reads it (its `review_ask`: anything else, and a later
    /// pass asks again), so a no that may not last (the day's cap, a hold,
    /// a draft) isn't taken for an ask; 2 for that no, 1 for a failure. The
    /// steward reads the JSON line instead.
    pub fn code(&self) -> i32 {
        match self {
            Self::Posted { .. } | Self::Asked { .. } => 0,
            Self::Failed { .. } => 1,
            Self::Declined { .. } => 2,
        }
    }
}

const PR: &str = "query($owner: String!, $name: String!, $n: Int!) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $n) {
      state isDraft headRefOid
      labels(first: 100) { pageInfo { hasNextPage } nodes { name } }
      reactionGroups { content reactors(first: 10) { edges { reactedAt node { __typename ... on Bot { login } ... on User { login } } } } }
      reviewThreads(first: 100) { pageInfo { hasNextPage } nodes { isResolved } }
      reviews(last: 100) { pageInfo { hasPreviousPage } nodes { author { login __typename } body commit { oid } submittedAt } }
      comments(last: 100) { pageInfo { hasPreviousPage } nodes {
        author { login __typename } body updatedAt
        reactionGroups { content reactors(first: 10) { edges { reactedAt node { __typename ... on Bot { login } ... on User { login } } } } }
      } }
    }
  }
}";

/// Ask the connector to review `repo`#`pr` at `head`, as the gh login `api`
/// runs as, if every check holds (the module's doc). `now` in seconds.
pub fn ask(api: &mut dyn Api, cfg: &Config, now: u64, repo: &str, pr: u64, head: &str) -> Outcome {
    let Some((owner, name)) = repo.split_once('/') else {
        return declined(format!("{repo:?} isn't OWNER/NAME"));
    };
    if head.len() != 40 || !head.bytes().all(|b| b.is_ascii_hexdigit()) {
        return declined(format!("{head:?} isn't a commit"));
    }
    if !cfg.repos.iter().any(|r| r.eq_ignore_ascii_case(repo)) {
        return declined(format!("{repo} isn't a repo the owner asks in"));
    }
    let _lock = match lock(&cfg.state_dir) {
        Ok(l) => l,
        Err(why) => return failed(why),
    };
    let path = cfg.state_dir.join("asked.json");
    let mut record = match load(&path) {
        Ok(r) => r,
        Err(why) => return failed(why),
    };
    let vars = json!({"owner": owner, "name": name, "n": pr});
    let data = match query(api, owner, PR, vars) {
        Ok(d) => d,
        Err(e) => return failed(format!("reading {repo}#{pr}: {}", e.why)),
    };
    let p = &data["repository"]["pullRequest"];
    let said = match look(cfg, p, head) {
        Ok(said) => said,
        Err(why) => return declined(why),
    };
    let key = |a: &Asked| {
        a.repo.eq_ignore_ascii_case(repo) && a.pr == pr && a.head == head && a.said == said
    };
    if let Some(a) = record.asked.iter().find(|a| key(a)) {
        return Outcome::Asked { at: a.at };
    }
    record.asked.retain(|a| now.saturating_sub(a.at) < KEEP);
    let today = record
        .asked
        .iter()
        .filter(|a| now.saturating_sub(a.at) < DAY)
        .count();
    if today >= cfg.per_day {
        return declined(format!("{today} asks in the last 24 hours, the day's cap"));
    }
    // Recorded first: whatever happens after, this head isn't asked twice.
    let asked = Asked {
        repo: repo.to_owned(),
        pr,
        head: head.to_owned(),
        said,
        at: now,
    };
    record.asked.push(asked.clone());
    if let Err(why) = save(&path, &record) {
        return failed(why);
    }
    match call(
        api,
        "POST",
        &format!(
            "repos/{}/{}/issues/{pr}/comments",
            encode(owner),
            encode(name)
        ),
        Some(&json!({"body": ASK})),
        None,
    ) {
        Ok(_) => Outcome::Posted { at: now },
        // No answer, or a 5xx: the comment may be up, so it stays asked.
        Err(e) if e.unanswered && e.retry_at.is_none() => failed(format!(
            "{} (kept as asked: GitHub may have taken it)",
            e.why
        )),
        // GitHub's no, a rate limit's included: nothing went up, so the
        // record goes and a later pass can ask.
        Err(e) => {
            record.asked.retain(|a| a != &asked);
            match save(&path, &record) {
                Ok(()) => failed(e.why),
                Err(why) => failed(format!("{}; and {why}", e.why)),
            }
        }
    }
}

/// The checks on the PR as GitHub has it: when Codex last said anything of
/// `head` (0: nothing) if it may be asked, else why not.
fn look(cfg: &Config, p: &Value, head: &str) -> Result<u64, String> {
    if p.is_null() {
        return Err("no such PR".into());
    }
    if p["state"] != "OPEN" {
        return Err(format!(
            "it's {}",
            p["state"].as_str().unwrap_or("not open").to_lowercase()
        ));
    }
    if p["isDraft"] != false {
        return Err("it's a draft".into());
    }
    if p["headRefOid"] != head {
        return Err(format!(
            "its head is {}, not {head}",
            p["headRefOid"].as_str().unwrap_or("unknown")
        ));
    }
    if p["labels"]["pageInfo"]["hasNextPage"] != false
        || p["reviews"]["pageInfo"]["hasPreviousPage"] != false
        || p["comments"]["pageInfo"]["hasPreviousPage"] != false
    {
        return Err("it has more labels, reviews or comments than one page shows".into());
    }
    let nodes = |v: &Value| v["nodes"].as_array().cloned().unwrap_or_default();
    if let Some(l) = nodes(&p["labels"])
        .iter()
        .filter_map(|l| l["name"].as_str())
        .find(|l| cfg.held.iter().any(|h| h == l))
    {
        return Err(format!("it's labelled {l}"));
    }
    let comments = nodes(&p["comments"]);
    // An ask in hand is the connector's 👀 on the PR or a comment since it
    // last posted anything on the PR: once it's done, it posts, and swaps
    // its 👀 for a 👍.
    let time = |v: &Value| v.as_str().map_or(Ok(0.0), burrow_upkeep::ts);
    let mut last = 0.0f64;
    for (v, at) in comments
        .iter()
        .map(|c| (c, "updatedAt"))
        .chain(nodes(&p["reviews"]).iter().map(|r| (r, "submittedAt")))
    {
        if login(&v["author"]) == CONNECTOR {
            last = last.max(time(&v[at])?);
        }
    }
    let mut in_hand = false;
    for v in std::iter::once(p).chain(comments.iter()) {
        for g in v["reactionGroups"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|g| g["content"] == "EYES")
        {
            for e in g["reactors"]["edges"].as_array().into_iter().flatten() {
                in_hand |= login(&e["node"]) == CONNECTOR && time(&e["reactedAt"])? > last;
            }
        }
    }
    if in_hand {
        return Err("Codex has an ask in hand (its 👀)".into());
    }
    let parse = |v: Value| serde_json::from_value(v).map_err(|e| format!("GitHub's answer: {e}"));
    let asked = verdict::Asked {
        head: head.to_owned(),
        reviews: parse(p["reviews"]["nodes"].clone())?,
        comments: comments
            .iter()
            .map(|c| verdict::Comment {
                author: c["author"].clone(),
                body: c["body"].as_str().map(String::from),
                updated_at: c["updatedAt"].as_str().map(String::from),
            })
            .collect(),
        edited: None,
        retargeted: None,
        pushed: None,
    };
    let said = verdict::said(&asked)?;
    match said.verdict {
        Verdict::None => {}
        Verdict::Running => return Err("Codex is reviewing this head".into()),
        Verdict::Clean => return Err("Codex found nothing in this head".into()),
        Verdict::Findings => {
            let threads = nodes(&p["reviewThreads"]);
            if p["reviewThreads"]["pageInfo"]["hasNextPage"] != false
                || threads.iter().any(|t| t["isResolved"] != true)
            {
                return Err(
                    "Codex reviewed this head, and its findings aren't all answered".into(),
                );
            }
        }
    }
    Ok(said.at as u64)
}

fn declined(why: impl Into<String>) -> Outcome {
    Outcome::Declined { why: why.into() }
}

fn failed(why: impl Into<String>) -> Outcome {
    Outcome::Failed { why: why.into() }
}

/// The record's lock, held while one ask reads, posts and records: asks
/// for two of a repo's PRs, or two repos', can run at once.
fn lock(dir: &Path) -> Result<File, String> {
    let path = dir.join("asked.lock");
    let f = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    f.lock()
        .map_err(|e| format!("locking {}: {e}", path.display()))?;
    Ok(f)
}

fn load(path: &Path) -> Result<Record, String> {
    match fs::read_to_string(path) {
        Ok(s) => serde_json::from_str(&s).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Record::default()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Written whole, then renamed into place: a reader sees the old record or
/// the new one, never half of one.
fn save(path: &Path, record: &Record) -> Result<(), String> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut tmp =
        tempfile::NamedTempFile::new_in(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let body = serde_json::to_vec_pretty(record).map_err(|e| e.to_string())?;
    tmp.write_all(&body)
        .and_then(|()| tmp.as_file().sync_all())
        .map_err(|e| format!("{}: {e}", path.display()))?;
    tmp.persist(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(())
}

pub mod broker;

#[cfg(test)]
mod tests;
