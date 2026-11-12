//! A question's subject. Jobs that ask about the same key share one notice:
//! the first with an open question leads and owns the notice, the rest wait on
//! its answer and add none. The registry lives in host state, outside every
//! job's writable files, and which job leads is read from it each time rather
//! than stored, so a leader that stops or is answered hands the notice on.
use super::{host_path, load};
use crate::{config::Config, job::Job, read, swap};
use serde::{Deserialize, Serialize};
use std::fs;

pub(crate) const FILE: &str = "question-about";
const MAX_KEY: usize = 120;

#[derive(Default, Serialize, Deserialize)]
struct Members {
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

/// Held while the registry is read and rewritten, so two jobs joining at once
/// cannot both lead.
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

fn load_members(cfg: &Config, key: &str) -> Vec<String> {
    read(&registry(cfg, key))
        .and_then(|s| serde_json::from_str::<Members>(&s).ok())
        .map(|m| m.members)
        .unwrap_or_default()
}

fn save_members(cfg: &Config, key: &str, members: Vec<String>) -> Result<(), String> {
    let path = registry(cfg, key);
    if members.is_empty() {
        let _ = fs::remove_file(path);
        return Ok(());
    }
    swap(
        &path,
        &serde_json::to_string(&Members { members }).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

pub(crate) fn key_of(job: &Job) -> Option<String> {
    read(&host_path(job, FILE))
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

/// Still waiting on an answer to this subject.
fn waiting(cfg: &Config, id: &str, key: &str) -> bool {
    let job = Job::new(cfg, id);
    key_of(&job).as_deref() == Some(key)
        && (load(&job).is_some() || host_path(&job, "question-needs-owner").exists())
}

fn lead(cfg: &Config, key: &str) -> Option<String> {
    load_members(cfg, key)
        .into_iter()
        .find(|id| waiting(cfg, id, key))
}

/// The job whose notice carries this job's question, when it is another's.
pub(crate) fn follows(job: &Job) -> Option<String> {
    let key = key_of(job)?;
    lead(job.cfg, &key).filter(|leader| leader != job.id)
}

/// The other jobs waiting on this job's answer, when it leads.
pub(crate) fn followers(job: &Job) -> Vec<String> {
    let Some(key) = key_of(job) else {
        return vec![];
    };
    let members = load_members(job.cfg, &key);
    match members.iter().find(|id| waiting(job.cfg, id, &key)) {
        Some(leader) if leader == job.id => members
            .into_iter()
            .filter(|id| id != job.id && waiting(job.cfg, id, &key))
            .collect(),
        _ => vec![],
    }
}

/// The deadline and one-way requirement a joiner takes from its leader.
pub(crate) fn leader_question(job: &Job) -> Option<super::Question> {
    load(&Job::new(job.cfg, &follows(job)?))
}

/// Record that this job's open question is about `key`. Call before its
/// question is written, so it is never taken for its own leader.
pub(crate) fn join(job: &Job, key: &str) -> Result<(), String> {
    let _lock = locked(job.cfg)?;
    super::ensure(job)?;
    swap(&host_path(job, FILE), key).map_err(|e| e.to_string())?;
    let mut members: Vec<String> = load_members(job.cfg, key)
        .into_iter()
        .filter(|id| id != job.id && waiting(job.cfg, id, key))
        .collect();
    members.push(job.id.to_owned());
    save_members(job.cfg, key, members)
}

/// The footer a leader's notice carries: who else waits on the same answer.
pub(crate) fn footer(job: &Job) -> String {
    let others = followers(job);
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
/// notice that stays. Their other notices close, linking to it.
pub(crate) fn merge(cfg: &Config, key: &str, ids: &[String]) -> Result<String, String> {
    let key = normalize(key)?;
    let _lock = locked(cfg)?;
    let mut open = Vec::new();
    for id in ids {
        let job = Job::new(cfg, id);
        if !job.dir.is_dir() {
            return Err(format!("no job {id}"));
        }
        if load(&job).is_none() && !host_path(&job, "question-needs-owner").exists() {
            return Err(format!("{id} has no open question to merge"));
        }
        let since = read(&host_path(&job, "question-since"))
            .and_then(|s| s.trim().parse::<i64>().ok())
            .unwrap_or(0);
        open.push((since, id.clone()));
    }
    open.sort_by(|a, b| b.cmp(a));
    open.dedup_by(|a, b| a.1 == b.1);
    for (_, id) in &open {
        let job = Job::new(cfg, id);
        super::ensure(&job)?;
        swap(&host_path(&job, FILE), &key).map_err(|e| e.to_string())?;
    }
    let leader = open[0].1.clone();
    save_members(cfg, &key, open.into_iter().map(|(_, id)| id).collect())?;
    crate::job::kick(cfg, ["proj-watch".to_owned(), "autoland".to_owned()]);
    Ok(leader)
}
