//! What a Claude job of the steward's that settles a PR leaves for the
//! steward to post (tasks.py `fix_task`): its replies on the PR's review
//! threads, each resolved after it, the PR's new description and a comment.
//! The job pushes and writes them to its answers file; once the job is over
//! the PR's pass reads it (claims.rs), and they go up as the steward's App
//! only while the PR's head is the job's own push, or the head it was handed
//! when it pushed nothing. So a job can't say it fixed something in a commit
//! GitHub doesn't have, and a job whose PR someone else moved on meanwhile
//! says nothing at all: the new head is theirs.
//!
//! `pusher` says who pushed a branch's head, from GitHub's own record of the
//! branch's pushes, so a pass can tell a job's own push from anyone else's
//! (claims.rs).

use std::collections::{BTreeMap, BTreeSet};

use burrow_github::{Error, call, call_once, encode, mutate, query};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::pass::Pass;
use crate::post::quiet;

/// The PR, what its job was handed, and what the job left.
#[derive(Debug)]
pub struct Handed {
    pub number: u64,
    /// The PR's head when the job was handed it.
    pub head: String,
    /// Its description then.
    pub description: String,
    /// When, in Unix seconds.
    pub at: f64,
    /// The job's answers file as it left it, or None for none.
    pub answers: Option<String>,
}

/// The answers file, as the job's brief asks for it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Answers {
    /// The commit it pushed last, or None when it pushed nothing.
    pushed: Option<String>,
    #[serde(default)]
    threads: Vec<Reply>,
    /// The whole new description, or None to leave it.
    #[serde(default)]
    description: Option<String>,
    /// A comment on the PR, or None.
    #[serde(default)]
    comment: Option<String>,
}

/// Its answer on one review thread: what it says, and whether its push
/// fixed what the thread found.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    id: String,
    fixed: bool,
    reply: String,
}

/// What went up.
#[derive(Debug, PartialEq, Eq)]
pub enum Posted {
    /// Its answers, every one.
    Answers,
    /// Nothing: it left no answers file.
    Nothing,
}

/// Why nothing (more) went up.
#[derive(Debug, PartialEq, Eq)]
pub enum Unposted {
    /// What it left isn't answers as its brief asks for them: the job ended
    /// without its answer.
    Wrong(String),
    /// It says it pushed a commit the PR's head isn't, and the head is still
    /// the one it was handed: its push never landed, and the job ended
    /// without its answer.
    Unpushed(String),
    /// Someone else moved the PR on since the job was handed it (a push,
    /// its description edited, closed): its answers are about a PR that
    /// isn't there any more, so it stands down.
    Moved(String),
    /// GitHub said no: the same answers can go again next pass, and what of
    /// them already went up isn't posted twice.
    Refused(String),
    /// GitHub didn't answer.
    Unanswered(String),
}

/// The PR as `answer` checks it: its state, head, description and review
/// threads, each with its last comment.
const PR: &str = "query($owner: String!, $name: String!, $n: Int!) { repository(owner: $owner, name: $name) { \
pullRequest(number: $n) { state headRefOid headRefName body \
reviewThreads(first: 100) { pageInfo { hasNextPage } nodes { id isResolved comments(last: 1) { nodes { body } } } } } } }";
const REPLY: &str = "mutation($t: ID!, $b: String!) { addPullRequestReviewThreadReply(input: {pullRequestReviewThreadId: $t, body: $b}) { comment { id } } }";
const RESOLVE: &str =
    "mutation($t: ID!) { resolveReviewThread(input: {threadId: $t}) { thread { isResolved } } }";

/// A commit as git names it in full.
fn commit(s: &str) -> bool {
    s.len() == 40
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn short(c: &str) -> &str {
    c.get(..12).unwrap_or(c)
}

/// GitHub keeps a description's and a comment's line ends as they came: one
/// written with \r\n is the same text.
fn same(a: &str, b: &str) -> bool {
    a.replace("\r\n", "\n").trim_end() == b.replace("\r\n", "\n").trim_end()
}

/// The answers file, or why it won't do.
fn read(h: &Handed, text: &str) -> Result<Answers, Unposted> {
    let a: Answers = serde_json::from_str(text).map_err(|e| {
        Unposted::Wrong(format!(
            "its answers file isn't what its brief asks for: {e}"
        ))
    })?;
    if let Some(c) = &a.pushed
        && !commit(c)
    {
        return Err(Unposted::Wrong(format!(
            "its answers say it pushed {c:?}, which isn't a commit's full name"
        )));
    }
    let new = a.pushed.as_deref().filter(|c| *c != h.head);
    let mut seen = BTreeSet::new();
    for r in &a.threads {
        if r.id.trim().is_empty() || r.reply.trim().is_empty() {
            return Err(Unposted::Wrong(
                "its answers have a thread with no id or no reply".into(),
            ));
        }
        if !seen.insert(r.id.as_str()) {
            return Err(Unposted::Wrong(format!(
                "its answers reply on thread {} twice",
                r.id
            )));
        }
        if r.fixed && new.is_none() {
            return Err(Unposted::Wrong(format!(
                "its answers say it fixed thread {}, but it pushed no new commit",
                r.id
            )));
        }
    }
    Ok(a)
}

/// What goes on a thread for its answer: the commit it's fixed in, which is
/// the PR's head (checked), never one the job named itself.
fn reply_text(r: &Reply, pushed: Option<&str>) -> String {
    let said = quiet(r.reply.trim());
    match pushed {
        Some(c) if r.fixed => format!("**Claude job** · fixed in {c}\n\n{said}"),
        _ => format!("**Claude job** · left as it is\n\n{said}"),
    }
}

fn comment_text(c: &str) -> String {
    format!("**Claude job**\n\n{}", quiet(c.trim()))
}

fn unposted(e: Error) -> Unposted {
    if e.unanswered {
        Unposted::Unanswered(e.why)
    } else {
        Unposted::Refused(e.why)
    }
}

/// The job's answers, posted if the PR is still where the job left it.
/// Everything is checked before anything goes up: a thread that isn't the
/// PR's, or a fix claimed with no new commit, posts none of it.
pub fn answer(p: &mut Pass, h: &Handed) -> Result<Posted, Unposted> {
    let Some(text) = &h.answers else {
        return Ok(Posted::Nothing);
    };
    let a = read(h, text)?;
    let (repo, n) = (p.repo, h.number);
    let (owner, name) = repo.split_once('/').unwrap_or((repo, ""));
    let data = query(
        p.api,
        owner,
        PR,
        json!({"owner": owner, "name": name, "n": n}),
    )
    .map_err(|e| unposted(p.noted(e)))?;
    let pr = &data["repository"]["pullRequest"];
    let (Some(state), Some(head)) = (pr["state"].as_str(), pr["headRefOid"].as_str()) else {
        return Err(Unposted::Refused(format!(
            "GitHub's answer has no PR #{n} with a head"
        )));
    };
    if state != "OPEN" {
        return Err(Unposted::Moved(format!(
            "it's {} now",
            state.to_lowercase()
        )));
    }
    let want = a.pushed.as_deref().unwrap_or(&h.head);
    if head != want {
        let branch = pr["headRefName"].as_str().unwrap_or("its branch");
        if let Some(c) = &a.pushed
            && head == h.head
        {
            return Err(Unposted::Unpushed(format!(
                "its push of {} never reached {branch}: the PR's head is still {}, the one it was handed",
                short(c),
                short(head)
            )));
        }
        return Err(Unposted::Moved(format!(
            "{branch} moved on to {}, which isn't the job's push",
            short(head)
        )));
    }
    // An answer can rest on the description (a reason it gives, held up):
    // edited since by anyone but this job's own earlier try, it's someone
    // else's PR now, whether the job writes one or not.
    let body = pr["body"].as_str().unwrap_or_default();
    let ours = a.description.as_deref().is_some_and(|new| same(new, body));
    if !same(body, &h.description) && !ours {
        return Err(Unposted::Moved(
            "its description was edited since the job was handed it".into(),
        ));
    }
    let description = a.description.as_ref().filter(|_| !ours);
    let threads: BTreeMap<&str, &Value> = pr["reviewThreads"]["nodes"]
        .as_array()
        .map(|ts| {
            ts.iter()
                .filter_map(|t| Some((t["id"].as_str()?, t)))
                .collect()
        })
        .unwrap_or_default();
    if let Some(r) = a
        .threads
        .iter()
        .find(|r| !threads.contains_key(r.id.as_str()))
    {
        let more = pr["reviewThreads"]["pageInfo"]["hasNextPage"] == json!(true);
        return Err(Unposted::Wrong(format!(
            "its answers reply on thread {}, which isn't one of the PR's{}",
            r.id,
            if more { " first 100" } else { "" }
        )));
    }
    let pushed = a.pushed.as_deref().filter(|c| *c != h.head);
    for r in &a.threads {
        let t = threads[r.id.as_str()];
        // Resolved since (by someone, or by this on an earlier try): it's
        // answered.
        if t["isResolved"] == json!(true) {
            continue;
        }
        let text = reply_text(r, pushed);
        let last = t["comments"]["nodes"][0]["body"].as_str();
        if !last.is_some_and(|l| same(l, &text)) {
            mutate(p.api, owner, REPLY, json!({"t": r.id, "b": text}))
                .map_err(|e| unposted(p.noted(e)))?;
        }
        mutate(p.api, owner, RESOLVE, json!({"t": r.id})).map_err(|e| unposted(p.noted(e)))?;
    }
    if let Some(new) = description {
        // Read again right before it's written over, and written once: one
        // asked again could undo an edit made in between, after a 429 too.
        // The next try reads it all again, and sees one whose answer was
        // lost as ours (above).
        let path = format!("repos/{repo}/pulls/{n}");
        let now = p.get(&path).map_err(unposted)?;
        if now["state"] != json!("open")
            || now["head"]["sha"].as_str() != Some(want)
            || !same(now["body"].as_str().unwrap_or_default(), body)
        {
            return Err(Unposted::Moved(
                "the PR moved on while its answers went up".into(),
            ));
        }
        call_once(p.api, "PATCH", &path, Some(&json!({"body": new})))
            .map_err(|e| unposted(p.noted(e)))?;
    }
    if let Some(c) = &a.comment {
        let text = comment_text(c);
        let since = jiff::Timestamp::from_second(h.at as i64)
            .map_or_else(|_| String::new(), |t| t.to_string());
        let path = format!(
            "repos/{repo}/issues/{n}/comments?since={}&per_page=100",
            encode(&since)
        );
        let me = p.me();
        let had = p.pages(&path).map_err(unposted)?.iter().any(|x| {
            x["user"]["login"].as_str() == Some(me.as_str())
                && x["body"].as_str().is_some_and(|b| same(b, &text))
        });
        if !had {
            let path = format!("repos/{repo}/issues/{n}/comments");
            call(p.api, "POST", &path, Some(&json!({"body": text})), None)
                .map_err(|e| unposted(p.noted(e)))?;
        }
    }
    eprintln!(
        "{repo}#{n}: a Claude job's answers posted, {} thread(s){}",
        a.threads.len(),
        pushed.map_or(String::new(), |c| format!(", on its push of {}", short(c)))
    );
    Ok(Posted::Answers)
}

/// A branch and its head.
#[derive(Debug)]
pub struct Head {
    pub branch: String,
    pub head: String,
}

/// Who pushed the branch's head, as GitHub's activity has it: the login of
/// the newest push or force push to the branch that left it at `head`
/// (`SLUG[bot]` for an App), or None when its newest pushes have none that
/// did.
pub fn pusher(p: &mut Pass, h: &Head) -> Result<Option<String>, Error> {
    let path = format!(
        "repos/{}/activity?ref={}&per_page=30",
        p.repo,
        encode(&format!("refs/heads/{}", h.branch))
    );
    let got = p.get(&path)?;
    let list = got
        .as_array()
        .ok_or_else(|| Error::said(format!("GET {path}: GitHub's answer isn't a list")))?;
    Ok(list
        .iter()
        .find(|x| {
            x["after"].as_str() == Some(h.head.as_str())
                && matches!(x["activity_type"].as_str(), Some("push" | "force_push"))
        })
        .and_then(|x| x["actor"]["login"].as_str())
        .map(String::from))
}

#[cfg(test)]
#[path = "answer_tests.rs"]
mod tests;
