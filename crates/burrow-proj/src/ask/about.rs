//! Shared questions: one atomic registry owns each published question's
//! subject, ordering and effective policy. Job locks precede the registry;
//! multi-job operations only try locks, never wait while holding another.
use super::{Question, host_path};
use crate::{config::Config, job::Job, read, swap};
use serde::{Deserialize, Serialize};
use std::fs;

mod state;
use state::{Group, Member, State};
pub(crate) const FILE: &str = "question-about"; // legacy host metadata
const MAX_KEY: usize = 120;

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

/// The registry's lock. Job locks precede it; held by every operation that
/// reads policy and commits on it (publication, merging, deadline shifts and
/// a reversible default's acceptance), so none acts on a stale snapshot.
pub(crate) fn locked(cfg: &Config) -> Result<fs::File, String> {
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

/// Legacy publications predate embedded identity. Bind them to the opened
/// file itself, never to the separately allocated question-id marker.
fn publication(job: &Job, name: &str) -> Option<Question> {
    use std::{io::Read, os::unix::fs::MetadataExt};
    let file = crate::state_reads::open(&host_path(job, name)).ok()?;
    let meta = file.metadata().ok()?;
    let mut text = String::new();
    file.take(64_001).read_to_string(&mut text).ok()?;
    if text.len() > 64_000 {
        return None;
    }
    let mut q: Question = serde_json::from_str(&text).ok()?;
    q.id.get_or_insert_with(|| format!("legacy:{}:{}", meta.dev(), meta.ino()));
    Some(q)
}

fn legacy_due(job: &Job) -> i64 {
    // Released legacy questions kept the exact deadline in their synthetic
    // completion. Prefer that over reconstructing it with today's timeout.
    let latest = job.turns().and_then(|n| n.parse::<u64>().ok()).unwrap_or(0);
    for n in (1..=latest).rev() {
        if let Some(result) = read(&job.turn(&n.to_string(), "result"))
            && let Some(time) = result
                .strip_prefix("No answer by ")
                .and_then(|s| s.split_once(". The recommended answer was:").map(|p| p.0))
            && let Some(epoch) = super::parse_utc(time)
        {
            return epoch;
        }
    }
    read(&host_path(job, "question-since"))
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0)
        .saturating_add(job.cfg.ask_timeout as i64)
}

/// Both forms of an unanswered question, with identity read from the same
/// publication as policy. Released legacy questions are always owner-required.
fn open_raw(job: &Job) -> Option<Question> {
    publication(job, "question").or_else(|| {
        let default = read(&host_path(job, "question-needs-owner"))?;
        if host_path(job, "question-released").exists() {
            return publication(job, "question-released");
        }
        use std::os::unix::fs::MetadataExt;
        let meta = fs::metadata(host_path(job, "question-needs-owner")).ok()?;
        Some(Question {
            id: Some(format!("legacy:{}:{}", meta.dev(), meta.ino())),
            recommendation: super::Recommendation {
                default,
                one_way: true,
                about: None,
            },
            due: legacy_due(job),
        })
    })
}

/// The one liveness rule: a question waits until an accepted answer (a
/// person's or the deadline's default) retires it, even while that answer's
/// cleanup is still pending. Membership, leadership and policy all read it.
pub(crate) fn identity(job: &Job) -> Option<String> {
    open_raw(job)?
        .id
        .filter(|s| !s.is_empty() && !crate::question::retiring(job, s))
}

/// The registry's view of a job's live question. A job with none needs no
/// registry, so only questions are held by its errors.
fn registry(job: &Job) -> Result<Option<State>, String> {
    if identity(job).is_none() {
        if open_raw(job).is_none()
            && (host_path(job, "question").exists()
                || host_path(job, "question-needs-owner").exists())
        {
            return Err(format!("{} has unreadable question authority", job.id));
        }
        return Ok(None);
    }
    State::load(job.cfg).map(Some)
}

pub(crate) fn key_of(job: &Job) -> Result<Option<String>, String> {
    let Some(state) = registry(job)? else {
        return Ok(None);
    };
    Ok(state.standing(job).map(|(key, _)| key.to_owned()))
}

/// The leader whose notice carries this job's question. Err when the registry
/// can't say: never read as "this job leads".
pub(crate) fn follows(job: &Job) -> Result<Option<String>, String> {
    let Some(state) = registry(job)? else {
        return Ok(None);
    };
    let Some((_, group)) = state.standing(job) else {
        return Ok(None);
    };
    Ok(state
        .live(job.cfg, group)
        .first()
        .map(|m| m.id.clone())
        .filter(|id| id != job.id))
}

/// Either form of the open question, a released one included, under its
/// effective policy. Err when its authority is there but can't be read.
pub(crate) fn open(job: &Job) -> Result<Option<Question>, String> {
    let Some(mut q) = open_raw(job) else {
        if host_path(job, "question").exists() || host_path(job, "question-needs-owner").exists() {
            return Err(format!("{} has unreadable question authority", job.id));
        }
        return Ok(None);
    };
    if q.id
        .as_deref()
        .is_some_and(|id| crate::question::retiring(job, id))
    {
        return Ok(None);
    }
    apply(job, &mut q)?;
    Ok(Some(q))
}

/// The unanswered, unreleased question under its effective policy. Err when
/// its authority is there but can't be read: never "no question".
pub(crate) fn load(job: &Job) -> Result<Option<Question>, String> {
    let Some(mut q) = publication(job, "question") else {
        if host_path(job, "question").exists() {
            return Err(format!("{} has unreadable question authority", job.id));
        }
        return Ok(None);
    };
    if q.id
        .as_deref()
        .is_some_and(|id| crate::question::retiring(job, id))
    {
        return Ok(None);
    }
    State::load(job.cfg)?.apply(job, &mut q)?;
    Ok(Some(q))
}

pub(crate) fn apply(job: &Job, q: &mut Question) -> Result<(), String> {
    if q.id.is_none() {
        q.id = identity(job);
    }
    State::load(job.cfg)?.apply(job, q)
}

/// The other jobs one reply to this leader resumes. Err when the registry
/// can't say, so a reply is refused rather than resuming the leader alone.
pub(crate) fn cohort(job: &Job) -> Result<Vec<(String, String)>, String> {
    let Some(state) = registry(job)? else {
        return Ok(vec![]);
    };
    let Some((_, group)) = state.standing(job) else {
        return Ok(vec![]);
    };
    let live = state.live(job.cfg, group);
    if live.first().map(|m| m.id.as_str()) != Some(job.id) {
        return Ok(vec![]);
    }
    Ok(live
        .into_iter()
        .skip(1)
        .map(|m| (m.id, m.question))
        .collect())
}

#[cfg(test)]
pub(crate) fn question_id(job: &Job) -> String {
    identity(job).unwrap_or_default()
}

pub(crate) fn still_waiting(job: &Job, key: &str, question: &str) -> Result<bool, String> {
    Ok(identity(job).as_deref() == Some(question) && key_of(job)?.as_deref() == Some(key))
}

pub(crate) fn publish(
    job: &Job,
    key: &str,
    mut q: Question,
    write: impl FnOnce(&Question) -> Result<(), String>,
) -> Result<(), String> {
    let _lock = locked(job.cfg)?;
    super::ensure(job)?;
    let mut state = State::load(job.cfg)?;
    // The caller read this job's policy before the lock. Whatever joined,
    // merged or shifted its group since binds the question it moves.
    if let Some((_, group)) = state.standing(job)
        && q.id.is_some()
        && q.id == identity(job)
    {
        q.due = group.due;
        q.recommendation.one_way |= group.one_way;
    }
    let before = state.0.get(key);
    let live = before.map(|g| state.live(job.cfg, g)).unwrap_or_default();
    let existing = live
        .iter()
        .any(|m| m.id == job.id && Some(&m.question) == q.id.as_ref());
    let mut next = Group {
        due: q.due,
        one_way: q.recommendation.one_way,
        members: live,
    };
    if !next.members.is_empty() {
        let before = before.unwrap();
        next.due = before.due;
        next.one_way |= before.one_way;
    }
    if !existing {
        next.members.retain(|m| m.id != job.id);
        next.members.push(Member {
            id: job.id.to_owned(),
            question: q.id.clone().ok_or("question has no identity")?,
        });
    }
    for group in state.0.values_mut() {
        group.members.retain(|m| m.id != job.id);
    }
    q.due = next.due;
    q.recommendation.one_way = next.one_way;
    state.0.insert(key.to_owned(), next);
    // Commit binding and policy before publication. Redeclarations retain the
    // identity, so even a crash here keeps their constraints authoritative.
    state.save(job.cfg)?;
    write(&q)
}

/// Move the question's effective deadline out to `due`, never in: its
/// group's when it has one (every member waits on it), else its own.
pub(crate) fn extend(job: &Job, due: i64) -> Result<(), String> {
    let _lock = locked(job.cfg)?;
    if load(job)?.is_none() {
        return Ok(());
    }
    let mut state = State::load(job.cfg)?;
    if let Some(key) = state.standing(job).map(|(key, _)| key.to_owned()) {
        let group = state.0.get_mut(&key).expect("standing names a group");
        if due > group.due {
            group.due = due;
            state.save(job.cfg)?;
        }
        return Ok(());
    }
    let path = host_path(job, "question");
    let Some(text) = read(&path) else {
        if path.exists() {
            return Err(format!("{} has unreadable question authority", job.id));
        }
        return Ok(());
    };
    let mut q: Question = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    if q.recommendation.about.is_some() || host_path(job, FILE).exists() {
        return Err(format!("{} has an unbound shared question", job.id));
    }
    if due > q.due {
        q.due = due;
        swap(
            &path,
            &serde_json::to_string(&q).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub(crate) fn footer(job: &Job) -> Result<String, String> {
    let others: Vec<String> = cohort(job)?.into_iter().map(|(id, _)| id).collect();
    if others.is_empty() {
        return Ok(String::new());
    }
    Ok(format!(
        "\n\nThe same question is waiting on this answer for {} other job{}: {}. One reply here (proj reply {}) resumes all of them.",
        others.len(),
        if others.len() == 1 { "" } else { "s" },
        others.join(", "),
        job.id
    ))
}

pub(crate) fn merge(cfg: &Config, key: &str, ids: &[String]) -> Result<String, String> {
    let key = normalize(key)?;
    let mut ids = ids.to_vec();
    ids.sort();
    ids.dedup();
    if ids.is_empty() {
        return Err("name at least one job to merge".into());
    }
    let mut held = Vec::new();
    for id in &ids {
        let job = Job::new(cfg, id);
        if !job.dir.is_dir() {
            return Err(format!("no job {id}"));
        }
        // Admission may hold any other job's lock while inspecting this one.
        // Never block here with a lock it could need: abandon this attempt.
        held.push(
            job.try_lock()
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("{id} is busy; retry the merge"))?,
        );
    }
    for id in &ids {
        crate::question::recover(&Job::new(cfg, id))?;
    }
    let _lock = locked(cfg)?;
    let mut state = State::load(cfg)?;
    let mut named = Vec::new();
    for id in &ids {
        let job = Job::new(cfg, id);
        let mut q = open_raw(&job).ok_or_else(|| format!("{id} has no open question to merge"))?;
        state.apply(&job, &mut q)?;
        let question = identity(&job).ok_or_else(|| format!("{id} has no question identity"))?;
        let since = read(&host_path(&job, "question-since"))
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0);
        named.push((
            since,
            Member {
                id: id.clone(),
                question,
            },
            q,
        ));
    }
    named.sort_by(|a, b| (b.0, &b.1.id).cmp(&(a.0, &a.1.id)));
    let before = state.0.get(&key);
    let live = before.map(|g| state.live(cfg, g)).unwrap_or_default();
    let due = if live.is_empty() {
        named[0].2.due
    } else {
        before.unwrap().due
    };
    let one_way = (!live.is_empty() && before.unwrap().one_way)
        || named.iter().any(|n| n.2.recommendation.one_way);
    let members = named
        .iter()
        .map(|n| n.1.clone())
        .chain(live.into_iter().filter(|m| !ids.contains(&m.id)))
        .collect();
    let leader = named[0].1.id.clone();
    for group in state.0.values_mut() {
        group.members.retain(|m| !ids.contains(&m.id));
    }
    state.0.insert(
        key,
        Group {
            due,
            one_way,
            members,
        },
    );
    // All source removals, destination membership and effective policies are
    // one rename. A failed or interrupted write exposes the complete old state.
    state.save(cfg)?;
    drop(_lock);
    drop(held);
    crate::job::kick(cfg, ["proj-watch".to_owned(), "autoland".to_owned()]);
    Ok(leader)
}
