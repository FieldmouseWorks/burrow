//! A PR that a first job opened and awaits goes first in the landing line.
//!
//! A job started with --first (someone's waiting on it), --pipeline (work
//! that keeps the pipeline moving) or --unstick (the stuck issue's) is first
//! from its start to its end: [`carried`]. When it ends a turn with
//! `proj await` on a PR it opened itself, proj puts the steward's `first`
//! label (burrow.autoland.labels.first, [`Config::first_label`]) on that PR,
//! so the steward's proof line and Conary's flow (priority_labels) take it
//! ahead of the rest. As the job's own identity, as its turns run gh
//! (its App's env, else the user's own login): never on a PR someone else
//! opened, nor on one its identity opened before the job began.
//! Only once the wait is registered, and on a budget of its own: a label
//! GitHub won't take, or takes long over, never fails or delays the await.

use std::time::{Duration, Instant, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::config::Config;
use crate::job::Job;
use crate::record::{JobFile, TurnFile};

/// How long one PR's look and label may take.
const BUDGET: Duration = Duration::from_secs(15);

/// Whether the job was started as first: --pipeline, --unstick, or a turn
/// of it given --first (`proj run --first`, `proj reply --first`). Not a
/// normal turn the line promoted after it waited past maxWait: that one is
/// first for its slot only, not for what it lands.
pub(crate) fn carried(job: &Job) -> bool {
    if job.file(JobFile::Pipeline).exists() || crate::record::unstuck(job) {
        return true;
    }
    let last = job.turns().and_then(|n| n.parse::<u64>().ok()).unwrap_or(0);
    (1..=last).any(|n| {
        crate::read(&job.turn_file(&n.to_string(), TurnFile::Priority)).as_deref() == Some("first")
    })
}

const ABOUT: &str = "query($owner: String!, $name: String!, $n: Int!) {
  repository(owner: $owner, name: $name) {
    issueOrPullRequest(number: $n) {
      __typename
      ... on PullRequest { id viewerDidAuthor createdAt labels(first: 100) { nodes { name } } }
    }
  }
}";

/// Put the first label on each of what the job now awaits (OWNER/REPO, N)
/// that's a PR it opened, if it carries first.
pub(crate) fn mark_all(cfg: &Config, job: &Job, awaited: &[(String, String)]) {
    if cfg.first_label.as_deref().is_some_and(|l| !l.is_empty()) && carried(job) {
        for (repo, n) in awaited {
            mark(cfg, job, repo, n);
        }
    }
}

fn mark(cfg: &Config, job: &Job, repo: &str, n: &str) {
    let Some(label) = cfg.first_label.as_deref() else {
        return;
    };
    let Some(mut gh) = as_job(cfg, job) else {
        return;
    };
    gh.until = Some(Instant::now() + BUDGET);
    // A GraphQL call names no repo: gh-token finds the account by this.
    gh.env.push(("GH_REPO".into(), repo.into()));
    let (Some((owner, name)), Ok(number)) = (repo.split_once('/'), n.parse::<u64>()) else {
        return;
    };
    let body = json!({"query": ABOUT, "variables": {"owner": owner, "name": name, "n": number}});
    let pr = match burrow_github::call_repeating(&mut gh, "POST", "graphql", Some(&body))
        .and_then(|r| r.json())
    {
        Ok(v) => v["data"]["repository"]["issueOrPullRequest"].clone(),
        Err(e) => return eprintln!("proj: couldn't read {repo}#{n} to label it {label}: {e}"),
    };
    if !opened(job, &pr) {
        return;
    }
    let has = pr["labels"]["nodes"]
        .as_array()
        .is_some_and(|l| l.iter().any(|x| x["name"] == label));
    if has {
        return;
    }
    let add = json!({"labels": [label]});
    let path = format!("repos/{repo}/issues/{n}/labels");
    if let Err(e) = burrow_github::call(&mut gh, "POST", &path, Some(&add), None) {
        eprintln!("proj: couldn't label {repo}#{n} {label}: {e}");
    }
}

/// Whether the PR is the job's own: its identity opened it, after the job
/// began (its first prompt was written).
fn opened(job: &Job, pr: &Value) -> bool {
    if pr["__typename"] != "PullRequest" || pr["viewerDidAuthor"] != true {
        return false;
    }
    let began = std::fs::metadata(job.prompt("1"))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64);
    let created = pr["createdAt"]
        .as_str()
        .and_then(|t| t.parse::<jiff::Timestamp>().ok())
        .map(|t| t.as_second());
    matches!((began, created), (Some(b), Some(c)) if c >= b)
}

/// gh as the job's turns run it: the user's gh, in its App's env when it
/// has one (as `engine::command` starts them). None when the job's App is
/// gone, or it has no gh of its own configured.
fn as_job(cfg: &Config, job: &Job) -> Option<burrow_github::Gh> {
    let job_gh = cfg.tools.job_gh.clone()?;
    let app = crate::job::Request::peek(job).ok()?.app;
    let env = match crate::turn::github_app(app) {
        Some(app) => cfg.app_environment(&app).filter(|e| !e.is_empty())?,
        None => String::new(),
    };
    let mut gh = crate::github::client(cfg);
    gh.program = Some(job_gh.into_os_string());
    gh.via = vec![
        cfg.tools.bash.clone().into_os_string(),
        "-c".into(),
        r#"eval "$PROJ_APP_ENV" && unset PROJ_APP_ENV && exec "$@""#.into(),
        "proj-first".into(),
    ];
    gh.env.push(("PROJ_APP_ENV".into(), env.into()));
    Some(gh)
}

#[cfg(test)]
mod tests;
