//! A reviewer queued on a PR's head: its snapshot taken (snapshot.rs), its
//! brief written (brief.rs) with its earlier findings and the replies they
//! got, and the entry left where the reviewers' service takes it
//! (burrow-review's `Item`).

use burrow_review::queue::Item;
use serde::Deserialize;
use serde_json::Value;

use crate::brief::brief;
use crate::pass::Pass;
use crate::post::finding;
use crate::snapshot::{self, Look};

/// Under the steward's state dir: the looks queued, the answers the
/// reviewers' service leaves under each one's name, and the reviewers'
/// sessions, a PR's each, which the service's config names too
/// (burrow-review's config.rs).
pub const QUEUE: &str = "review/queue";
pub const DONE: &str = "review/done";
pub const SESSIONS: &str = "review/sessions";

/// What the pass hands `queue`, on stdin: the PR's head on its base, as the
/// pass read it, and the reviewer to queue on it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ask {
    pub number: u64,
    pub head: String,
    pub base: String,
    pub title: String,
    /// Its description as the reviewers are shown it, each line quoted
    /// (common.py `quote`), and what an answer carries of it
    /// (`fingerprint`).
    pub quoted: String,
    pub description: String,
    /// When the PR was opened, which its place in the queue goes by.
    pub opened: f64,
    /// The reviewer as its marks name it (`codex`).
    pub reviewer: String,
    /// The head's snapshot's name (reviewers.py `snap_name`).
    pub name: String,
    /// How a reviewer weighs the reasons a description gives
    /// (reviewers.py `REASONS`, which a judge's task gives too).
    pub reasons: String,
}

impl Ask {
    /// The snapshot it reads.
    pub fn look(&self) -> Look {
        Look {
            number: self.number,
            head: self.head.clone(),
            base: self.base.clone(),
            name: self.name.clone(),
        }
    }

    /// Why it can't be queued as given, if it can't: a look that isn't one,
    /// or a reviewer whose name isn't lowercase letters (it names the entry
    /// and the marks).
    pub fn wrong(&self) -> Option<String> {
        self.look().wrong().or_else(|| {
            (self.reviewer.is_empty() || !self.reviewer.bytes().all(|b| b.is_ascii_lowercase()))
                .then(|| format!("{:?} isn't a reviewer's name", self.reviewer))
        })
    }
}

/// The reviewer queued on the head's snapshot, briefed with the PR's
/// description as the pass read it: one edited to answer a finding is what
/// the next look at the same head reads, and its fingerprint goes back with
/// the answer. The entry's name, or why it couldn't be queued (the head
/// moved on, GitHub didn't answer).
pub fn queue(p: &mut Pass, ask: &Ask) -> Result<String, String> {
    let meta = snapshot::take(p.cfg, &p.state_dir(), p.repo, &ask.look())?;
    let earlier = prior(p, ask.number, &ask.reviewer)?;
    let item = Item {
        reviewer: ask.reviewer.clone(),
        snap: ask.name.clone(),
        repo: p.repo.to_string(),
        number: ask.number,
        head_ref_oid: ask.head.clone(),
        base_ref_name: ask.base.clone(),
        prompt: brief(&p.cfg.owner, p.repo, ask, &meta, &earlier),
        description: ask.description.clone(),
        opened: ask.opened,
        at: p.now,
    };
    let entry = format!("{}--{}", ask.reviewer, ask.name);
    p.save(&format!("{QUEUE}/{entry}.json"), &item)?;
    Ok(entry)
}

/// The reviewer's earlier findings on the PR, the steward's threads with
/// its mark, each with the replies it got from the steward's authors and
/// the steward: anyone else's text never steers it. Read whole, or not at
/// all: a brief without them would have it raise answered findings again.
pub fn prior(p: &mut Pass, number: u64, reviewer: &str) -> Result<String, String> {
    let all = p
        .pages(&format!(
            "repos/{}/pulls/{number}/comments?per_page=100",
            p.repo
        ))
        .map_err(|e| format!("its earlier findings: {}", e.why))?;
    let me = p.me();
    let mark = finding(reviewer);
    let text = |c: &Value, k: &str| c[k].as_str().unwrap_or_default().to_string();
    let login = |c: &Value| c["user"]["login"].as_str().unwrap_or_default().to_string();
    let mut out = vec![];
    for root in all
        .iter()
        .filter(|c| login(c) == me && text(c, "body").contains(&mark))
    {
        let at = root["line"]
            .as_u64()
            .or_else(|| root["original_line"].as_u64())
            .map(|n| format!(":{n}"))
            .unwrap_or_default();
        let mut said = format!(
            "- {}{at}: {}\n",
            text(root, "path"),
            text(root, "body").replace(&mark, "").trim()
        );
        for reply in all.iter().filter(|x| x["in_reply_to_id"] == root["id"]) {
            let who = login(reply);
            if who == me || p.ours(&who) {
                said += &format!("  Reply from {who}: {}\n", text(reply, "body").trim());
            }
        }
        out.push(said);
    }
    Ok(out.join("\n"))
}

#[cfg(test)]
#[path = "queue_tests.rs"]
mod tests;
