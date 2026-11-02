//! What a run remembers of trees it built and VM-tested, for a day: a tree
//! that failed to build isn't tried again, nor a VM test run again on a
//! tree it passed or failed. A forced run (`--force`) remembers nothing.

use std::fs::{self, OpenOptions};
use std::io::Write;

use crate::status::epoch;
use crate::{Broke, Config};

/// How long a tree's build or VM test is remembered.
const DAY: i64 = 86_400;

/// How a VM test went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Fail,
}

impl Verdict {
    fn word(self) -> &'static str {
        match self {
            Verdict::Pass => "pass",
            Verdict::Fail => "fail",
        }
    }
}

pub struct Memo<'a> {
    pub cfg: &'a Config,
    pub force: bool,
}

impl Memo<'_> {
    /// Whether HEAD's tree failed `what` (a build) in the last day.
    pub fn failed_before(&self, what: &str) -> bool {
        if self.force {
            return false;
        }
        let Some(tree) = self.tree("HEAD") else {
            return false;
        };
        let key = format!("{what}:{tree}");
        let since = epoch() - DAY;
        lines(self.cfg, "failed-trees").any(|l| {
            let mut f = l.split_whitespace();
            f.next() == Some(key.as_str())
                && f.next()
                    .and_then(|t| t.parse::<i64>().ok())
                    .is_some_and(|t| t > since)
        })
    }

    /// That HEAD's tree failed `what`, now.
    pub fn failed(&self, what: &str) -> Result<(), Broke> {
        let tree = self.tree("HEAD").unwrap_or_default();
        append(
            self.cfg,
            "failed-trees",
            &format!("{what}:{tree} {}", epoch()),
        )
    }

    /// How the VM test `check` went on `commit`'s tree in the last day, its
    /// last result when it ran more than once: nothing when it didn't run.
    pub fn result_of(&self, check: &str, commit: &str) -> Option<Verdict> {
        if self.force {
            return None;
        }
        let key = format!("{check}:{}", self.tree(commit)?);
        let since = epoch() - DAY;
        // As awk's `$1 == k && $3 > since { r = $2 }` has it: the last
        // matching line's word, which may be neither.
        lines(self.cfg, "check-results")
            .filter_map(|l| {
                let f: Vec<&str> = l.split_whitespace().collect();
                let fresh = f
                    .get(2)
                    .and_then(|t| t.parse::<i64>().ok())
                    .is_some_and(|t| t > since);
                (f.first() == Some(&key.as_str()) && fresh).then(|| match f.get(1) {
                    Some(&"pass") => Some(Verdict::Pass),
                    Some(&"fail") => Some(Verdict::Fail),
                    _ => None,
                })
            })
            .last()
            .flatten()
    }

    /// That the VM test `check` went `result` on `commit`'s tree, now.
    pub fn record(&self, check: &str, commit: &str, result: Verdict) -> Result<(), Broke> {
        let tree = self.tree(commit).unwrap_or_default();
        append(
            self.cfg,
            "check-results",
            &format!("{check}:{tree} {} {}", result.word(), epoch()),
        )
    }

    fn tree(&self, commit: &str) -> Option<String> {
        self.cfg
            .git(&["rev-parse", &format!("{commit}^{{tree}}")])
            .ok()
    }
}

/// How many runs in a row the VM test `check` has passed first time, now one
/// more (`passed`) or none.
pub fn clean(cfg: &Config, check: &str, passed: bool) -> Result<u32, Broke> {
    let name = format!("clean-{check}");
    let n = if passed {
        cfg.recorded(&name).parse::<u32>().unwrap_or(0) + 1
    } else {
        0
    };
    fs::write(cfg.state.join(&name), format!("{n}\n"))
        .map_err(|e| Broke(format!("{name}: {e}")))?;
    Ok(n)
}

fn lines<'a>(cfg: &Config, name: &str) -> impl Iterator<Item = String> + 'a {
    fs::read_to_string(cfg.state.join(name))
        .unwrap_or_default()
        .lines()
        .map(String::from)
        .collect::<Vec<_>>()
        .into_iter()
}

fn append(cfg: &Config, name: &str, line: &str) -> Result<(), Broke> {
    OpenOptions::new()
        .append(true)
        .create(true)
        .open(cfg.state.join(name))
        .and_then(|mut f| writeln!(f, "{line}"))
        .map_err(|e| Broke(format!("{name}: {e}")))
}

#[cfg(test)]
#[path = "memo_tests.rs"]
mod tests;
