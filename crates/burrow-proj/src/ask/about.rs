//! A question's subject. Jobs that ask about the same key share one notice:
//! the first member still waiting leads and owns the notice, the rest wait on
//! its answer and add none. The registry lives in host state, outside every
//! job's writable files. It holds the group's order and its policy (deadline,
//! one-way), so a member joining after the leader timed out still inherits
//! them. Who leads is read from it each time rather than stored, so a leader
//! that stops or is answered hands the notice on.
//!
//! Lock order: a job's lock, then the registry's. Readers take neither.
use super::{Question, host_path, load_raw};
use crate::{config::Config, job::Job, read, swap};
use serde::{Deserialize, Serialize};
use std::fs;

pub(crate) const FILE: &str = "question-about";
const MAX_KEY: usize = 120;

#[derive(Default, Serialize, Deserialize)]
struct Group {
    due: i64,
    one_way: bool,
    members: Vec<String>,
}

/// One spelling per subject: trimmed, single-spaced, lowercase.
pub(crate) fn normalize(raw: &str) -> Result<String, String> {
    let key = raw
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    if key.is_empty() || key.len() > MAX_KEY || key.chars().any(char::is_control) {
        return Err(format!(
            "--about takes a short subject key (1 to {MAX_KEY} characters, one line)"
        ));
    }
    Ok(key)
}

fn dir(cfg: &Config) -> std::path::PathBuf {
    super::control_root(cfg).join(".about")
}

fn registry(cfg: &Config, key: &str) -> std::path::PathBuf {
    let hex: String = key.bytes().map(|b| format!("{b:02x}")).collect();
    dir(cfg).join(hex)
}

/// Held while the registry is read and rewritten, so joins, merges and the
/// question writes they belong to are one step to every other job.
fn locked(cfg: &Config) -> Result<fs::File, String> {
    use std::os::unix::fs::DirBuilderExt;
    super::ensure_root(cfg)?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir(cfg))
        .map_err(|e| e.to_string())?;
    let file = fs::File::options()
        .create(true)
        .append(true)
        .open(dir(cfg).join("lock"))
        .map_err(|e| e.to_string())?;
    file.lock().map_err(|e| e.to_string())?;
    Ok(file)
}

fn group(cfg: &Config, key: &str) -> Option<Group> {
    serde_json::from_str(&read(&registry(cfg, key))?).ok()
}

fn save(cfg: &Config, key: &str, group: &Group) -> Result<(), String> {
    let path = registry(cfg, key);
    if group.members.is_empty() {
        let _ = fs::remove_file(path);
        return Ok(());
    }
    swap(
        &path,
        &serde_json::to_string(group).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

pub(crate) fn key_of(job: &Job) -> Option<String> {
    read(&host_path(job, FILE))
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

/// Still waiting on an answer to this subject, timed out for the owner too.
fn waiting(cfg: &Config, id: &str, key: &str) -> bool {
    let job = Job::new(cfg, id);
    key_of(&job).as_deref() == Some(key)
        && (load_raw(&job).is_some() || host_path(&job, "question-needs-owner").exists())
}

fn waiting_members(cfg: &Config, key: &str, group: &Group) -> Vec<String> {
    group
        .members
        .iter()
        .filter(|id| waiting(cfg, id, key))
        .cloned()
        .collect()
}

/// This job's key, its group, and who waits in it: only for a registered member.
fn standing(job: &Job) -> Option<(Group, Vec<String>)> {
    let key = key_of(job)?;
    let group = group(job.cfg, &key)?;
    group.members.contains(&job.id.to_owned()).then_some(())?;
    let live = waiting_members(job.cfg, &key, &group);
    Some((group, live))
}

/// The job whose notice carries this job's question, when it is another's.
pub(crate) fn follows(job: &Job) -> Option<String> {
    let (_, live) = standing(job)?;
    live.into_iter().next().filter(|leader| leader != job.id)
}

/// The group's deadline and requirement, over this job's own.
pub(crate) fn apply(job: &Job, q: &mut Question) {
    if let Some((group, _)) = standing(job) {
        q.due = group.due;
        q.recommendation.one_way |= group.one_way;
    }
}

/// Jobs waiting on this job's answer when it leads, each with the question it
/// was asking, so an answer reaches only questions that were open when given.
pub(crate) fn cohort(job: &Job) -> Vec<(String, String)> {
    let Some((_, live)) = standing(job) else {
        return vec![];
    };
    if live.first().map(String::as_str) != Some(job.id) {
        return vec![];
    }
    live.into_iter()
        .filter(|id| id != job.id)
        .map(|id| {
            let question = question_id(&Job::new(job.cfg, &id));
            (id, question)
        })
        .collect()
}

pub(crate) fn question_id(job: &Job) -> String {
    read(&host_path(job, "question-id")).unwrap_or_default()
}

/// Still the question, about the subject, that was open when the answer came.
pub(crate) fn still_waiting(job: &Job, key: &str, question: &str) -> bool {
    key_of(job).as_deref() == Some(key)
        && question_id(job) == question
        && (load_raw(job).is_some() || host_path(job, "question-needs-owner").exists())
}

/// Publish this job's question under `key`: its place in the group, the
/// group's policy over its own, and the question itself, as one step under the
/// registry's lock. A redeclared open question keeps its place.
pub(crate) fn publish(
    job: &Job,
    key: &str,
    mut q: Question,
    write: impl FnOnce(&Question) -> Result<(), String>,
) -> Result<(), String> {
    let _lock = locked(job.cfg)?;
    super::ensure(job)?;
    let before = group(job.cfg, key).unwrap_or_default();
    let was_waiting = waiting(job.cfg, job.id, key) && before.members.iter().any(|m| m == job.id);
    let others: Vec<String> = waiting_members(job.cfg, key, &before)
        .into_iter()
        .filter(|id| id != job.id)
        .collect();
    let mut next = Group {
        due: q.due,
        one_way: q.recommendation.one_way,
        members: vec![],
    };
    if !others.is_empty() {
        next.due = before.due;
        next.one_way |= before.one_way;
    }
    next.members = if was_waiting {
        // Same question again: same place, even behind or ahead of others.
        before
            .members
            .iter()
            .filter(|id| *id == job.id || others.contains(id))
            .cloned()
            .collect()
    } else {
        others.into_iter().chain([job.id.to_owned()]).collect()
    };
    q.due = next.due;
    q.recommendation.one_way = next.one_way;
    // Registered first: a crash before the question is written leaves a
    // member that is not waiting, which the next join drops.
    save(job.cfg, key, &next)?;
    swap(&host_path(job, FILE), key).map_err(|e| e.to_string())?;
    write(&q)
}

/// The footer a leader's notice carries: who else waits on the same answer.
pub(crate) fn footer(job: &Job) -> String {
    let others: Vec<String> = cohort(job).into_iter().map(|(id, _)| id).collect();
    if others.is_empty() {
        return String::new();
    }
    format!(
        "\n\nThe same question is waiting on this answer for {} other job{}: {}. One reply here (proj reply {}) resumes all of them.",
        others.len(),
        if others.len() == 1 { "" } else { "s" },
        others.join(", "),
        job.id
    )
}

/// The one-time pass for questions asked before keys existed: put these jobs'
/// open questions under one subject, the newest leading so its wording is the
/// notice that stays. Their other notices close, linking to it. Members the
/// subject already has stay in it, behind the ones named.
pub(crate) fn merge(cfg: &Config, key: &str, ids: &[String]) -> Result<String, String> {
    let key = normalize(key)?;
    let mut ids: Vec<String> = ids.to_vec();
    ids.sort();
    ids.dedup();
    // Job locks first, in id order, then the registry's.
    let mut held = Vec::new();
    for id in &ids {
        let job = Job::new(cfg, id);
        if !job.dir.is_dir() {
            return Err(format!("no job {id}"));
        }
        held.push(job.lock().map_err(|e| e.to_string())?);
    }
    let _lock = locked(cfg)?;
    let mut named = Vec::new();
    for id in &ids {
        let job = Job::new(cfg, id);
        let Some(q) = load_raw(&job) else {
            return Err(format!("{id} has no open question to merge"));
        };
        let since = read(&host_path(&job, "question-since"))
            .and_then(|s| s.trim().parse::<i64>().ok())
            .unwrap_or(0);
        named.push((since, id.clone(), q));
    }
    named.sort_by(|a, b| (b.0, &b.1).cmp(&(a.0, &a.1)));
    let before = group(cfg, &key).unwrap_or_default();
    let kept: Vec<String> = waiting_members(cfg, &key, &before)
        .into_iter()
        .filter(|id| !ids.contains(id))
        .collect();
    let leader = named[0].1.clone();
    let joined = Group {
        due: if kept.is_empty() {
            named[0].2.due
        } else {
            before.due
        },
        one_way: before.one_way || named.iter().any(|n| n.2.recommendation.one_way),
        members: named.iter().map(|n| n.1.clone()).chain(kept).collect(),
    };
    for (_, id, _) in &named {
        let job = Job::new(cfg, id);
        super::ensure(&job)?;
        swap(&host_path(&job, FILE), &key).map_err(|e| e.to_string())?;
    }
    save(cfg, &key, &joined)?;
    drop(held);
    crate::job::kick(cfg, ["proj-watch".to_owned(), "autoland".to_owned()]);
    Ok(leader)
}
