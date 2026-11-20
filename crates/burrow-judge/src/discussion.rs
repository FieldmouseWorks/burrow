//! What a judge reads of a PR on GitHub, through Conary's own reads and
//! helpers, never a copy: the PR as it is now, held to what a request
//! names, and its discussion (every review thread and review body), whose
//! digest is the request's `context`.
use conary_github::api::PullRequest;
use conary_github::flow::{
    self, Read, ReviewThread, Revision, ThreadComment, judging_context, review_bodies,
};
use std::collections::BTreeMap;
use std::fmt::Write as _;

/// PR `pr` of `repo` as GitHub has it now, and its revision, once it's
/// open, on `head`, with `against` its title and description as the flow
/// joins them: or why it's moved on.
pub fn current(
    source: &dyn Read,
    repo: &str,
    pr: u64,
    head: &str,
    against: &str,
) -> Result<(PullRequest, Revision), String> {
    let pull = source
        .pull(repo, pr)
        .map_err(|e| format!("{repo}#{pr} can't be read: {e}"))?;
    if pull.state != "open" {
        return Err(format!("{repo}#{pr} isn't open"));
    }
    if pull.head.sha != head {
        return Err(format!("{head} isn't {repo}#{pr}'s head"));
    }
    let now = format!("{}\n{}", pull.title, pull.body.as_deref().unwrap_or(""));
    if now != against {
        return Err(format!(
            "{repo}#{pr}'s title or description isn't the one the judgment was asked on"
        ));
    }
    let revision = Revision::from(&pull);
    Ok((pull, revision))
}

/// A PR's discussion, as a judge reads it.
#[derive(Debug, Clone, Default)]
pub struct Discussion {
    pub threads: BTreeMap<String, ReviewThread>,
    pub reviews: BTreeMap<u64, ThreadComment>,
}

impl Discussion {
    /// Read whole, once: what `flow::read_judging_context` reads, kept
    /// with its digest, so what a judge is shown is what the context says.
    pub fn read(source: &dyn Read, repo: &str, pr: u64) -> Result<Self, String> {
        let failed = |e: conary_github::ApiError| format!("{repo}#{pr}'s discussion: {e}");
        Ok(Self {
            threads: source.snapshot(repo, pr).map_err(failed)?.threads,
            reviews: review_bodies(&source.reviews(repo, pr).map_err(failed)?),
        })
    }

    /// Its `flow::judging_context`.
    pub fn context(&self) -> String {
        judging_context(&self.threads, &self.reviews)
    }

    /// All of it, each comment quoted line by line under who wrote it, as
    /// a judge's brief shows it.
    pub fn quoted(&self) -> String {
        let said = |c: &ThreadComment| {
            format!(
                "{} wrote:\n{}\n",
                c.author.as_deref().unwrap_or("(someone deleted)"),
                burrow_brief::quote(&c.body)
            )
        };
        let mut out = String::new();
        for (id, thread) in &self.threads {
            let resolved = if thread.resolved { ", resolved" } else { "" };
            let _ = writeln!(out, "Review thread {id}{resolved}:");
            for c in &thread.comments {
                out.push_str(&said(c));
            }
            out.push('\n');
        }
        for (id, review) in &self.reviews {
            let _ = writeln!(out, "Review {id}'s body:");
            out.push_str(&said(review));
            out.push('\n');
        }
        if out.is_empty() {
            "(none)".into()
        } else {
            out.trim_end().to_owned()
        }
    }
}

/// The highest generation any judgment on `comments` names, by an account
/// among `authors`: authenticated or not, since a number already taken by
/// anyone the flow reads verdicts from is never issued again.
pub fn highest_generation(
    comments: &[conary_github::api::Comment],
    authors: &std::collections::BTreeSet<String>,
) -> u64 {
    comments
        .iter()
        .filter(|c| c.user.as_ref().is_some_and(|u| authors.contains(&u.login)))
        .flat_map(|c| c.body.as_deref().unwrap_or("").lines())
        .filter_map(|line| {
            let rest = line
                .strip_prefix("<!-- autoland-judgment:v2 ")
                .or_else(|| line.strip_prefix("<!-- autoland-judgment:v3 "))?;
            let json: serde_json::Value = serde_json::from_str(rest.strip_suffix(" -->")?).ok()?;
            json["issued"]["generation"].as_u64()
        })
        .max()
        .unwrap_or(0)
}

/// The tree of `head` in `repo`.
pub fn tree(source: &dyn Read, repo: &str, head: &str) -> Result<String, String> {
    source
        .tree(repo, head)
        .map_err(|e| format!("{head}'s tree in {repo}: {e}"))
}

/// Conary's `read_judging_context`, as the producer calls it.
pub fn context(source: &dyn Read, repo: &str, pr: u64) -> Result<String, String> {
    flow::read_judging_context(source, repo, pr)
        .map_err(|e| format!("{repo}#{pr}'s judging context: {e}"))
}
