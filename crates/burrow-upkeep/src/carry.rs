//! Which of a PR's earlier heads' looks cover its head: those whose change,
//! merged onto the base the head now leaves from, gives the head's tree
//! exactly. That's a merge of the base into the PR (or a rebase onto it, or
//! an empty commit) that brought nothing of the PR's own: git's own merge
//! puts each of its edits where it was, so the same lines moved elsewhere,
//! a conflict resolved by editing them, or a commit since, is another
//! change, which the reviewers look at. verdict.rs reads the looks at the
//! heads this names as covering the head.
//!
//! Every earlier head the steward posted a look at, on this base, is
//! checked, findings and all: a newer look with findings stops an older
//! clean one carrying. One that can't be checked (gone from the mirror)
//! might be such a look, so then nothing carries. The pass keeps the
//! answer for the head and hands it back (`kept`), so a head is worked out
//! once for each set of looks; a status run only reads it.

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::config::{self, Config};
use crate::mirror::{self, git_in};
use crate::snapshot::Changed;
use crate::verdict::{Review, marks, named};

/// What the pass hands `carry`, on stdin: the PR as it is now.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ask {
    pub number: u64,
    pub head: String,
    pub base: String,
    /// The reviewers, as their marks name them.
    pub reviewers: Vec<String>,
    /// Its reviews, as GraphQL gives them (prs.py).
    pub reviews: Vec<Review>,
    /// What `carry` said of the PR last, as the pass kept it.
    #[serde(default)]
    pub kept: Option<Carry>,
}

/// What the head carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Carry {
    pub head: String,
    pub base: String,
    /// The earlier heads the steward posted looks at, every one checked.
    pub heads: Vec<String>,
    /// Those whose looks cover the head.
    pub carried: Vec<String>,
    /// What the head changes that the reviewers aren't shown (snapshot.rs
    /// `Changed`), for a judge: no snapshot of the head is taken when its
    /// reviewers' looks carry, and none is needed for what it hides.
    pub unseen: Vec<String>,
}

impl Ask {
    /// Why it can't be read as given.
    pub fn wrong(&self) -> Option<String> {
        if let Some(r) = self.reviewers.iter().find(|r| !named(r)) {
            return Some(format!("{r:?} isn't a reviewer's name"));
        }
        (!mirror::id(&self.head)).then(|| format!("{:?} isn't a commit's id", self.head))
    }
}

/// The earlier heads `me`'s looks on the PR were of, on its base, and
/// whether any of those looks found nothing: with none, nothing carries.
fn looked(me: &str, ask: &Ask) -> Result<(Vec<String>, bool), String> {
    let (mut heads, mut clean) = (BTreeSet::new(), false);
    for r in &ask.reviewers {
        for m in marks(me, &ask.reviews, r)? {
            if let Some(of) = m.of
                && of != ask.head
                && m.base.as_deref() == Some(&ask.base)
            {
                clean |= !m.found;
                heads.insert(of);
            }
        }
    }
    Ok((heads.into_iter().collect(), clean))
}

/// What the PR's head carries, from the steward's mirror of the repo, with
/// the head and the base fetched into it now: None when no clean look
/// could carry. What was kept is the answer while the head, its base and
/// the looks are the same; a status run (`dry`) works out nothing else.
pub fn carry(
    cfg: &Config,
    repo: &str,
    me: &str,
    ask: &Ask,
    dry: bool,
) -> Result<Option<Carry>, String> {
    let (heads, clean) = looked(me, ask)?;
    if !clean {
        return Ok(None);
    }
    if let Some(k) = &ask.kept
        && k.head == ask.head
        && k.base == ask.base
        && k.heads == heads
    {
        return Ok(Some(k.clone()));
    }
    if dry {
        return Ok(None);
    }
    let base = &ask.base;
    let more = [format!("+refs/heads/{base}:refs/base/{base}")];
    let m = mirror::fetch_pr(cfg, &cfg.state_dir, repo, ask.number, &ask.head, &more)?;
    let at = At {
        m: &m,
        base: &format!("refs/base/{base}"),
        head: &ask.head,
    };
    let carried = at.carried(&heads)?;
    let unseen = if carried.is_empty() {
        vec![]
    } else {
        at.unseen(&cfg.snapshot, repo)?
    };
    Ok(Some(Carry {
        head: ask.head.clone(),
        base: base.clone(),
        heads,
        carried,
        unseen,
    }))
}

/// A head on a base, in the repo at `m`.
pub struct At<'a> {
    pub m: &'a Path,
    /// The base's ref.
    pub base: &'a str,
    pub head: &'a str,
}

fn line(out: &[u8]) -> String {
    String::from_utf8_lossy(out).trim().to_string()
}

impl At<'_> {
    /// Those of `heads` whose looks cover the head; none if one of them
    /// can't be checked.
    pub fn carried(&self, heads: &[String]) -> Result<Vec<String>, String> {
        let m = self.m;
        let mb = line(&git_in(m).args(["merge-base", self.base, self.head]).run()?);
        let tree = line(
            &git_in(m)
                .arg("rev-parse")
                .arg(format!("{}^{{tree}}", self.head))
                .run()?,
        );
        let mut carried = vec![];
        for h in heads {
            match self.carries(&mb, &tree, h)? {
                Some(true) => carried.push(h.clone()),
                Some(false) => {}
                None => return Ok(vec![]),
            }
        }
        Ok(carried)
    }

    /// Whether `h`'s change, merged onto `mb` (where the head leaves the
    /// base) by git's own merge, is the head's tree `tree`: None if `h`
    /// can't be read.
    fn carries(&self, mb: &str, tree: &str, h: &str) -> Result<Option<bool>, String> {
        let m = self.m;
        if !mirror::id(h)
            || git_in(m)
                .args(["rev-parse", "--verify", "-q"])
                .arg(format!("{h}^{{commit}}"))
                .run_or_1()?
                .is_none()
        {
            return Ok(None);
        }
        // Exit 1: nothing in common with the base, not a change on it.
        let Some(left) = git_in(m).args(["merge-base", self.base, h]).run_or_1()? else {
            return Ok(Some(false));
        };
        // Exit 1: it conflicts, so the head's resolution is a change of its
        // own.
        let merged = git_in(m)
            .args(["merge-tree", "--write-tree", "--no-messages"])
            .arg(format!("--merge-base={}", line(&left)))
            .args([mb, h])
            .run_or_1()?;
        Ok(Some(
            merged.is_some_and(|t| line(&t).lines().next() == Some(tree)),
        ))
    }

    /// What the head changes that the reviewers aren't shown.
    pub fn unseen(&self, cfg: &config::Snapshot, repo: &str) -> Result<Vec<String>, String> {
        let mb = line(
            &git_in(self.m)
                .args(["merge-base", self.base, self.head])
                .run()?,
        );
        Ok(Changed::read(cfg, repo, self.m, &mb, self.head)?.secret())
    }
}

#[cfg(test)]
#[path = "carry_tests.rs"]
mod tests;
