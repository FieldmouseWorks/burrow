//! When a task list's child is done: a merged PR is tied to it
//! (docs/task-planning.md). Tied is a closing reference from any repo, the
//! PR the child's own job awaited before it said `proj done`, or one an
//! author recorded with `proj plan done`. The last two are kept as the
//! child's `landed` file in its plan folder, which only the host writes: the
//! inbox from the job's own records, and `proj plan done` outside any job.
use super::{List, Pass};
use crate::{Fail, job::Job, read, swap};
use serde_json::{Value, json};
use std::fs;

/// The child's landed PR, as the host recorded it.
const LANDED: &str = "landed";

/// `OWNER/REPO#N`, as `proj await` and `proj plan done` take one.
fn reference(s: &str) -> Option<(String, u64)> {
    let (repo, n) = s.trim().split_once('#')?;
    let (owner, name) = repo.split_once('/')?;
    let part = |p: &str| {
        !p.is_empty()
            && p.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    };
    (part(owner) && part(name))
        .then(|| {
            n.parse()
                .ok()
                .filter(|n| *n > 0)
                .map(|n| (repo.to_owned(), n))
        })
        .flatten()
}

/// `https://github.com/OWNER/REPO/pull/N` as `OWNER/REPO#N`.
fn pull_url(s: &str) -> Option<(String, u64)> {
    let path = s.trim().strip_prefix("https://github.com/")?;
    let (repo, n) = path.trim_end_matches('/').rsplit_once("/pull/")?;
    reference(&format!("{repo}#{n}"))
}

impl Pass<'_> {
    /// The merged PR tied to child N: Some(Some(url)) when there is one,
    /// Some(None) when there's none, None when GitHub couldn't be asked.
    pub(super) fn landed_child(&self, repo: &str, n: u64) -> Option<Option<String>> {
        if let Some(pr) = self.recorded_landing(repo, n) {
            return Some(Some(pr));
        }
        // A child split again is done when its own list is.
        if let Ok(bytes) = fs::read(self.plan_dir(repo, &n.to_string()).join("tasks.json"))
            && let Ok(list) = serde_json::from_slice::<List>(&bytes)
            && !list.children.is_empty()
        {
            return Some(
                list.next()
                    .is_none()
                    .then(|| format!("https://github.com/{repo}/issues/{n}")),
            );
        }
        // A closing reference, from whichever repo.
        let (owner, name) = repo.split_once('/')?;
        let query = "query($owner:String!,$name:String!,$n:Int!){repository(owner:$owner,name:$name){issue(number:$n){closedByPullRequestsReferences(first:100,includeClosedPrs:true){nodes{merged url}}}}}";
        let got = crate::github::query_for(
            self.cfg,
            repo,
            query,
            json!({"owner":owner,"name":name,"n":n}),
        )?;
        Some(
            got["repository"]["issue"]["closedByPullRequestsReferences"]["nodes"]
                .as_array()?
                .iter()
                .find(|pr| pr["merged"] == true)
                .and_then(|pr| pr["url"].as_str())
                .map(str::to_owned),
        )
    }

    pub(super) fn recorded_landing(&self, repo: &str, n: u64) -> Option<String> {
        read(&self.plan_dir(repo, &n.to_string()).join(LANDED)).filter(|pr| !pr.is_empty())
    }

    fn record_landing(&self, repo: &str, n: u64, pr: &str) -> bool {
        let d = self.plan_dir(repo, &n.to_string());
        fs::create_dir_all(&d).is_ok() && swap(&d.join(LANDED), &format!("{pr}\n")).is_ok()
    }

    /// Whether N is a child on any of the repo's task lists.
    pub(super) fn is_child(&self, repo: &str, n: u64) -> bool {
        self.plan_dirs(repo).iter().any(|d| {
            fs::read(d.join("tasks.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<List>(&bytes).ok())
                .is_some_and(|list| list.children.iter().any(|c| c.number == Some(n)))
        })
    }

    /// Child N's own job said `proj done`: the newest PR any of its turns
    /// awaited that has merged, in whichever repo, becomes the child's
    /// landed PR. Some(None) when none has, None when GitHub couldn't be
    /// asked.
    pub(super) fn record_awaited(&self, repo: &str, n: u64, job: &Job) -> Option<Option<String>> {
        let turns: u64 = job.turns().and_then(|t| t.parse().ok()).unwrap_or(0);
        for turn in (1..=turns).rev() {
            let text = read(&job.turn(&turn.to_string(), "github-awaits")).unwrap_or_default();
            for line in text.lines().rev() {
                let Some((at, m)) = reference(line.split('\t').next().unwrap_or_default()) else {
                    continue;
                };
                let issue = crate::github::api(self.cfg, &format!("repos/{at}/issues/{m}"))?;
                if let Some(pr) = merged(&issue) {
                    return self.record_landing(repo, n, &pr).then_some(Some(pr));
                }
            }
        }
        Some(None)
    }

    /// `proj plan done`'s work: `caller` is the GitHub account it runs as.
    pub(crate) fn plan_done(
        &self,
        child: &str,
        pr: &str,
        caller: Option<u64>,
    ) -> Result<String, Fail> {
        let usage = || Fail::new(2, PLAN_USAGE);
        let (repo, n) = reference(child).ok_or_else(usage)?;
        let (at, m) = pull_url(pr).ok_or_else(usage)?;
        if !self.allowed(caller) {
            return Err(Fail::new(
                1,
                "proj plan done: only one of the inbox's authors may record a child's landed PR, and the GitHub account this runs as isn't one",
            ));
        }
        if !self.is_child(&repo, n) {
            return Err(Fail::new(
                1,
                format!("proj plan done: {repo}#{n} isn't a child on any task list"),
            ));
        }
        let issue =
            crate::github::api(self.cfg, &format!("repos/{at}/issues/{m}")).ok_or_else(|| {
                Fail::new(1, format!("proj plan done: couldn't read {pr} from GitHub"))
            })?;
        let url = merged(&issue).ok_or_else(|| {
            Fail::new(
                1,
                format!("proj plan done: {pr} hasn't merged, so nothing is recorded"),
            )
        })?;
        if !self.record_landing(&repo, n, &url) {
            return Err(Fail::new(
                1,
                format!("proj plan done: couldn't record {repo}#{n}'s landed PR"),
            ));
        }
        Ok(repo)
    }
}

/// A merged PR's URL, from its issue as REST gives it.
fn merged(issue: &Value) -> Option<String> {
    let pr = issue.get("pull_request")?;
    pr["merged_at"].as_str().filter(|t| !t.is_empty())?;
    pr["html_url"]
        .as_str()
        .or_else(|| issue["html_url"].as_str())
        .map(str::to_owned)
}

pub(crate) const PLAN_USAGE: &str = "usage: proj plan done OWNER/REPO#N PR-URL (an author records the merged PR that landed a task list's child)";

#[cfg(test)]
mod parse_tests {
    use super::*;

    #[test]
    fn references_and_pull_urls() {
        assert_eq!(reference("sam/box#8"), Some(("sam/box".into(), 8)));
        assert_eq!(reference("sam/box#0"), None);
        assert_eq!(reference("sam#8"), None);
        assert_eq!(reference("sam/b x#8"), None);
        assert_eq!(
            pull_url("https://github.com/sam/box/pull/10"),
            Some(("sam/box".into(), 10))
        );
        assert_eq!(pull_url("https://github.com/sam/box/issues/10"), None);
        assert_eq!(pull_url("https://example.com/sam/box/pull/10"), None);
    }
}
