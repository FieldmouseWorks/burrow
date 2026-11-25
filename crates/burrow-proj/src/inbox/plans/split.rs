//! Pre-flight validation and recovery for `split` proposals.
use super::{List, Retained};
use crate::inbox::Pass;
use crate::planning::Task;
use std::fs;

/// Why a `split` proposal could not be applied yet.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::inbox) enum SplitFail {
    /// Transient GitHub or local error: retry on a later pass.
    Retry,
    /// The split itself is invalid and cannot succeed on retry.
    Invalid(String),
}

impl Pass<'_> {
    /// Validate every reused existing issue (`task.issue`) before mutating
    /// `tasks.json` or creating any child issue.
    pub(in crate::inbox) fn validate_split_issues(
        &self,
        repo: &str,
        n: &str,
        tasks: &[Task],
    ) -> Result<(), SplitFail> {
        for (i, task) in tasks.iter().enumerate() {
            let Some(existing) = task.issue else {
                continue;
            };
            if existing.to_string() == n {
                return Err(SplitFail::Invalid(format!(
                    "task {} cannot reuse parent issue #{n}",
                    i + 1
                )));
            }
            let Some(response) = crate::github::reported(burrow_github::call_allowing(
                &mut crate::github::client(self.cfg),
                "GET",
                &format!("repos/{repo}/issues/{existing}"),
                None,
                None,
                &[404, 410],
            )) else {
                return Err(SplitFail::Retry);
            };
            if matches!(response.status, 404 | 410) {
                return Err(SplitFail::Invalid(format!(
                    "task {} references #{existing}, which does not exist in {repo}",
                    i + 1
                )));
            }
            let Some(issue) = crate::github::reported(response.json()) else {
                return Err(SplitFail::Retry);
            };
            if issue.get("pull_request").is_some() {
                return Err(SplitFail::Invalid(format!(
                    "task {} references #{existing}, which is a pull request, not an issue",
                    i + 1
                )));
            }
            if !self.eligible(repo, &issue) {
                return Err(SplitFail::Invalid(format!(
                    "task {} references #{existing}, which is not an eligible issue in {repo}",
                    i + 1
                )));
            }
        }
        Ok(())
    }

    /// Clear an incomplete active task list that matches a rejected split
    /// (for example, written before pre-flight validation existed), while
    /// preserving every retained and attempted creation receipt.
    pub(in crate::inbox) fn abort_incomplete_split(&self, repo: &str, n: &str, tasks: &[Task]) {
        let d = self.plan_dir(repo, n);
        let Ok(bytes) = fs::read(d.join("tasks.json")) else {
            return;
        };
        let Ok(mut list) = serde_json::from_slice::<List>(&bytes) else {
            return;
        };
        if list.tasks != tasks || !list.children.iter().any(|c| c.number.is_none()) {
            return;
        }
        for (task, child) in std::mem::take(&mut list.tasks)
            .into_iter()
            .zip(std::mem::take(&mut list.children))
        {
            if child.attempted || child.merged_pr.is_some() {
                list.retained.push(Retained { task, child });
            }
        }
        list.source = None;
        if self.save_list(repo, n, &list) {
            let _ = fs::remove_file(d.join("input"));
        }
    }

    pub(in crate::inbox) fn clear_worker_split(&self, repo: &str, n: &str) {
        let _ = fs::remove_file(self.plan_dir(repo, n).join("worker-split"));
    }
}
