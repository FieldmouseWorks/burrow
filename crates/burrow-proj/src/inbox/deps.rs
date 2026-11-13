//! The drain's dependency facts: which issues block which, read mechanically
//! from GitHub's issue dependencies and from explicit `Blocks:` /
//! `Blocked by:` lines in bodies, and what ranks a blocker first. Every drain
//! decision reads them afresh; a target that is closed stops counting.
//!
//! A candidate is a *blocker* when it blocks an urgent target, any other
//! issue that is open: an open `ready` issue, or an open pull request whose
//! head branch starts with one of `proj.inbox.drainUrgentBranches` (a red
//! default branch's fix, a deploy's update branch). The order the rules
//! apply in is `proj.inbox.drainOrder`.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use super::drain::Candidate;
use super::{Pass, raw};
use crate::github::{api, every};

/// An issue or pull request, by repository (lowercase) and number.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Ref {
    pub(crate) repo: String,
    pub(crate) n: u64,
}

impl Ref {
    pub(crate) fn new(repo: &str, n: u64) -> Self {
        Self {
            repo: repo.to_ascii_lowercase(),
            n,
        }
    }

    /// The issue a REST item is, from its `repository_url` and `number`.
    fn of(item: &Value) -> Option<Self> {
        let url = raw(&item["repository_url"]);
        let named = url.rsplit('/').take(2).collect::<Vec<_>>();
        let [name, owner] = named[..] else {
            return None;
        };
        Some(Self::new(
            &format!("{owner}/{name}"),
            item["number"].as_u64()?,
        ))
    }
}

/// What a body says, in `Blocks:` and `Blocked by:` lines.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Links {
    pub(crate) blocks: BTreeSet<Ref>,
    pub(crate) blocked_by: BTreeSet<Ref>,
}

/// The explicit lines of a body, `home` being the repository a bare `#N`
/// means. A line is `Blocks:` or `Blocked by:` (any case, after a list
/// marker or bold), then `#N`, `OWNER/REPO#N` or an issue or pull URL.
pub(crate) fn parse(body: &str, home: &str) -> Links {
    let mut links = Links::default();
    for line in body.lines() {
        let line = line
            .trim_start_matches(|c: char| c.is_whitespace() || "-*>".contains(c))
            .replace("**", "");
        let lower = line.to_ascii_lowercase();
        let (to, rest) = if let Some(r) = lower.strip_prefix("blocked by") {
            (&mut links.blocked_by, r)
        } else if let Some(r) = lower.strip_prefix("blocks") {
            (&mut links.blocks, r)
        } else {
            continue;
        };
        let Some(rest) = rest.trim_start().strip_prefix(':') else {
            continue;
        };
        for token in rest.split(|c: char| c.is_whitespace() || c == ',') {
            let token = token.trim_matches(|c: char| "()[].;".contains(c));
            if let Some(r) = reference(token, home) {
                to.insert(r);
            }
        }
    }
    links
}

fn reference(token: &str, home: &str) -> Option<Ref> {
    if let Some(n) = token.strip_prefix('#') {
        return Some(Ref::new(home, n.parse().ok()?));
    }
    if let Some((repo, n)) = token.split_once('#') {
        return burrow_github::repo_name(repo)
            .then(|| n.parse().ok().map(|n| Ref::new(repo, n)))?;
    }
    let path = token
        .strip_prefix("https://github.com/")
        .or_else(|| token.strip_prefix("http://github.com/"))?;
    let parts: Vec<&str> = path.split('/').collect();
    let [owner, name, "issues" | "pull", n] = parts[..] else {
        return None;
    };
    Some(Ref::new(&format!("{owner}/{name}"), n.parse().ok()?))
}

/// What the issues blocked by `of` are, per GitHub's dependency API, every
/// page of it; None where the API can't be read (it isn't there, or GitHub
/// won't say), the bodies being the fallback then.
pub(crate) fn blocking(cfg: &crate::config::Config, of: &Ref) -> Option<BTreeSet<Ref>> {
    let path = format!(
        "repos/{}/issues/{}/dependencies/blocking?per_page=100",
        of.repo, of.n
    );
    Some(every(cfg, &path)?.iter().filter_map(Ref::of).collect())
}

impl Pass<'_> {
    /// Candidates with `blocker` set where they block an urgent target, in
    /// the order `proj.inbox.drainOrder` sets, ties ending on repository and
    /// number. `items` are the ready issues the candidates came from (every
    /// ready one, claimed or not: blocking one qualifies).
    pub(super) fn order(&mut self, mut cands: Vec<Candidate>, items: &[Value]) -> Vec<Candidate> {
        // Unreadable or absent settings pause every repo, so nothing ranks.
        let Some(Ok(order)) = self.cfg.settings.as_ref().map(|s| s.drain_order()) else {
            return cands;
        };
        // One candidate has nothing to be ranked against.
        if cands.len() > 1 && order.iter().any(|r| r == "blockers") {
            let (universe, listed) = self.urgent_targets(items);
            let links: BTreeMap<&Ref, Links> = universe
                .iter()
                .map(|(r, b)| (r, parse(b, &r.repo)))
                .collect();
            // The pull requests were just listed open.
            let mut open: BTreeMap<Ref, bool> = listed.into_iter().map(|r| (r, true)).collect();
            for c in &mut cands {
                let me = Ref::new(&c.repo, c.n.parse().unwrap_or(0));
                let mut targets = blocking(self.cfg, &me).unwrap_or_default();
                if let Some(own) = links.get(&me) {
                    targets.extend(own.blocks.iter().cloned());
                }
                targets.extend(
                    links
                        .iter()
                        .filter(|(_, l)| l.blocked_by.contains(&me))
                        .map(|(r, _)| (*r).clone()),
                );
                c.blocker = targets.iter().any(|t| {
                    *t != me
                        && universe.contains_key(t)
                        && *open.entry(t.clone()).or_insert_with(|| self.is_open(t))
                });
            }
        }
        cands.sort_by(|a, b| {
            order
                .iter()
                .map(|rule| match rule.as_str() {
                    "blockers" => b.blocker.cmp(&a.blocker),
                    "priority" => a.priority.cmp(&b.priority),
                    "pipeline" => a.outside.cmp(&b.outside),
                    _ => a.opened.cmp(&b.opened),
                })
                .find(|o| o.is_ne())
                .unwrap_or(Ordering::Equal)
                .then_with(|| {
                    a.repo
                        .to_ascii_lowercase()
                        .cmp(&b.repo.to_ascii_lowercase())
                })
                .then_with(|| number(&a.n).cmp(&number(&b.n)))
        });
        cands
    }

    /// The issues and pull requests a blocker is ranked for blocking, by
    /// body, with the pull requests among them: every ready issue, and each project's open pull requests from an
    /// urgent branch.
    fn urgent_targets(&self, items: &[Value]) -> (BTreeMap<Ref, String>, Vec<Ref>) {
        let mut listed = Vec::new();
        let mut found: BTreeMap<Ref, String> = items
            .iter()
            .filter_map(|i| Some((Ref::of(i)?, raw(&i["body"]))))
            .collect();
        let prefixes = self
            .cfg
            .settings
            .as_ref()
            .and_then(|s| s.drain_urgent_branches().ok())
            .unwrap_or_default();
        if prefixes.is_empty() {
            return (found, listed);
        }
        for repo in self.project.keys() {
            let Some(pulls) = every(
                self.cfg,
                &format!("repos/{repo}/pulls?state=open&per_page=100"),
            ) else {
                continue;
            };
            for pull in pulls {
                let branch = raw(&pull["head"]["ref"]);
                if let Some(n) = pull["number"].as_u64()
                    && prefixes.iter().any(|p| branch.starts_with(p.as_str()))
                {
                    let r = Ref::new(repo, n);
                    listed.push(r.clone());
                    found.insert(r, raw(&pull["body"]));
                }
            }
        }
        (found, listed)
    }

    /// Whether GitHub has the issue or pull request open now; one it won't
    /// say about doesn't count.
    fn is_open(&self, r: &Ref) -> bool {
        api(self.cfg, &format!("repos/{}/issues/{}", r.repo, r.n))
            .is_some_and(|i| i["state"] == "open")
    }
}

fn number(n: &str) -> u64 {
    n.parse().unwrap_or(u64::MAX)
}
