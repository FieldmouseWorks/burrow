//! Pre-flight validation, idempotent rejection, and recovery for `split` proposals.
use super::{List, Retained};
use crate::inbox::Pass;
use crate::inbox::record::files;
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

/// Partial child-creation receipts preserved from an aborted split (either a
/// pre-existing incomplete plan or a post-binding retry whose reused issue
/// became invalid before finalization completed).
#[derive(Debug, Default, PartialEq, Eq)]
pub(in crate::inbox) struct RetainedPartial {
    pub(in crate::inbox) created: Vec<u64>,
    pub(in crate::inbox) unconfirmed: usize,
}

fn list_matches_split(list: &List, input: &str, tasks: &[Task]) -> bool {
    !list.tasks.is_empty()
        && list.tasks == tasks
        && (list.source.as_deref() == Some(input)
            || list.children.iter().any(|c| c.number.is_none()))
}

impl RetainedPartial {
    pub(in crate::inbox) fn is_empty(&self) -> bool {
        self.created.is_empty() && self.unconfirmed == 0
    }
}

pub(in crate::inbox) fn invalid_split_marker(
    repo: &str,
    n: &str,
    job_id: &str,
    turn: Option<&str>,
) -> String {
    match turn {
        Some(t) => format!("<!-- proj-invalid-split {repo}#{n} {job_id} turn-{t} -->"),
        None => format!("<!-- proj-invalid-split {repo}#{n} {job_id} -->"),
    }
}

pub(in crate::inbox) fn invalid_split_message(reason: &str, partial: &RetainedPartial) -> String {
    if partial.is_empty() {
        return format!("Invalid split: {reason}. No children created; needs replanning.");
    }
    let mut parts = Vec::new();
    if !partial.created.is_empty() {
        let nums: Vec<String> = partial.created.iter().map(|n| format!("#{n}")).collect();
        let noun = if partial.created.len() == 1 {
            "partial child"
        } else {
            "partial children"
        };
        parts.push(format!("{noun} {}", nums.join(", ")));
    }
    if partial.unconfirmed > 0 {
        let noun = if partial.unconfirmed == 1 {
            "unconfirmed child-creation attempt"
        } else {
            "unconfirmed child-creation attempts"
        };
        parts.push(format!("{} {noun}", partial.unconfirmed));
    }
    format!(
        "Invalid split: {reason}. Retained {} from an earlier attempt; no further children will be created; needs replanning.",
        parts.join(" and ")
    )
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

    /// Post a marked inbox comment idempotently: reconcile any existing comment
    /// carrying the same marker and inbox signature on GitHub before posting.
    pub(in crate::inbox) fn post_marked_once(
        &self,
        repo: &str,
        n: &str,
        marker: &str,
        body: &str,
    ) -> bool {
        let Some(comments) = crate::github::every(
            self.cfg,
            &format!("repos/{repo}/issues/{n}/comments?per_page=100"),
        ) else {
            return false;
        };
        if comments.iter().any(|c| {
            c["body"]
                .as_str()
                .is_some_and(|b| b.contains(marker) && b.contains(&self.cfg.inbox_mark))
        }) {
            return true;
        }
        self.post(repo, n, &format!("{body}\n\n{marker}"))
    }

    /// Inspect an active task list belonging to a rejected split (same source
    /// `input`, or incomplete with unbound children) for any partial
    /// child-creation receipts that will be retained on abort.
    pub(in crate::inbox) fn incomplete_split_partial(
        &self,
        repo: &str,
        n: &str,
        input: &str,
        tasks: &[Task],
    ) -> RetainedPartial {
        let d = self.plan_dir(repo, n);
        let Ok(bytes) = fs::read(d.join("tasks.json")) else {
            return RetainedPartial::default();
        };
        let Ok(list) = serde_json::from_slice::<List>(&bytes) else {
            return RetainedPartial::default();
        };
        if !list_matches_split(&list, input, tasks) {
            return RetainedPartial::default();
        }
        let mut partial = RetainedPartial::default();
        for child in &list.children {
            if child.attempted || child.merged_pr.is_some() {
                match child.number {
                    Some(num) => partial.created.push(num),
                    None => partial.unconfirmed += 1,
                }
            }
        }
        partial
    }

    /// Clear an active task list belonging to a rejected split (same source
    /// `input`, or incomplete with unbound children — for example, a
    /// pre-existing partial plan or a post-binding retry), while preserving
    /// every retained and attempted creation receipt.
    pub(in crate::inbox) fn abort_incomplete_split(
        &self,
        repo: &str,
        n: &str,
        input: &str,
        tasks: &[Task],
    ) -> bool {
        let d = self.plan_dir(repo, n);
        let bytes = match fs::read(d.join("tasks.json")) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return true,
            Err(_) => return false,
        };
        let Ok(mut list) = serde_json::from_slice::<List>(&bytes) else {
            return false;
        };
        if !list_matches_split(&list, input, tasks) {
            return true;
        }
        // Remove the input approval before emptying tasks.json: while the
        // nonempty task list remains on disk, has_task_list blocks worker
        // admission even if removing `input` fails and is retried.
        match files::mark(&d, files::File::Input).remove() {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(_) => return false,
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
        self.save_list(repo, n, &list)
    }

    pub(in crate::inbox) fn clear_worker_split(&self, repo: &str, n: &str) {
        let _ = fs::remove_file(self.plan_dir(repo, n).join("worker-split"));
    }
}
