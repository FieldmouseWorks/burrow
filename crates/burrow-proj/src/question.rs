//! Outstanding questions belong to the job, across every source of resumes.
//! Prepare the state before launching an engine, and restore it if the turn
//! cannot start. A new engine must never see yesterday's question as open
//! after a human answered, or lose one because a machinery start failed.

use std::fs;
use std::path::{Path, PathBuf};

use crate::{job::Job, read, swap};

const OPEN_UPDATE: &str = "Your open question is still posted. This update is not a person's reply. Do not repeat the question or ping the owner again. Handle the update, then run `proj ask --default \"TEXT\" [--one-way]` again with the same recommendation without restating the question if it still needs an answer.";

const LEGACY_OPEN_UPDATE: &str = "Your open question is still posted. This update is not a person's reply. Do not repeat the question or ping the owner again. Handle the update, then run `proj ask` again without restating the question if it still needs an answer.";

fn read_state(path: &Path) -> Result<Option<String>, String> {
    match crate::state_reads::bytes(path)
        .and_then(|b| String::from_utf8(b).map_err(std::io::Error::other))
    {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

pub(crate) struct Resume {
    path: PathBuf,
    before: Option<String>,
    boundary: PathBuf,
    boundary_before: Option<String>,
    asking: bool,
    committed: bool,
    metadata: Vec<(PathBuf, String)>,
}

impl Resume {
    pub(crate) fn prepare(job: &Job, answered: bool) -> Result<Self, String> {
        if !answered && crate::ask::host_path(job, "question-needs-owner").exists() {
            return Err("the one-way question is still waiting for a person's answer; machine news cannot resume this completed job".into());
        }
        let path = job.path("open-question");
        let before = match crate::state_reads::bytes(&path)
            .and_then(|b| String::from_utf8(b).map_err(std::io::Error::other))
        {
            Ok(s) => Some(s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let end = job.turns().and_then(|n| read(&job.turn(&n, "end")));
        let asking = !answered
            && (crate::ask::load(job).is_some()
                || (end.as_deref() != Some("done")
                    && (before.is_some() || end.as_deref() == Some("ask"))));
        let mut metadata = Vec::new();
        if !asking {
            for name in [
                "question",
                "question-needs-owner",
                "question-text",
                "question-since",
                "question-id",
            ] {
                let path = crate::ask::host_path(job, name);
                match crate::state_reads::bytes(&path)
                    .and_then(|b| String::from_utf8(b).map_err(std::io::Error::other))
                {
                    Ok(text) => metadata.push((path, text)),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                    Err(e) => return Err(format!("{}: {e}", path.display())),
                }
            }
        }
        let boundary = job.path("question-answered-through");
        let boundary_before = read_state(&boundary)?;
        let state = Self {
            boundary,
            boundary_before,
            path,
            before: before.clone(),
            asking,
            committed: false,
            metadata,
        };
        for (path, _) in &state.metadata {
            fs::remove_file(path).map_err(|e| e.to_string())?;
        }
        if answered && let Some(n) = job.turns() {
            swap(&state.boundary, &n).map_err(|e| e.to_string())?;
        }
        state.write(if asking {
            state.before.as_deref().or(Some("ask\n"))
        } else {
            None
        })?;
        Ok(state)
    }

    pub(crate) fn prompt(&self, msg: &str) -> String {
        if self.asking {
            format!("{msg}\n\n{OPEN_UPDATE}")
        } else {
            msg.to_owned()
        }
    }

    pub(crate) fn commit(mut self) {
        self.committed = true;
    }

    fn write(&self, value: Option<&str>) -> Result<(), String> {
        let result = if let Some(value) = value {
            swap(&self.path, value)
        } else {
            match fs::remove_file(&self.path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                result => result,
            }
        };
        result.map_err(|e| format!("{}: {e}", self.path.display()))
    }
}

impl Drop for Resume {
    fn drop(&mut self) {
        if !self.committed {
            for (path, text) in &self.metadata {
                if let Err(e) = swap(path, text) {
                    eprintln!("proj: could not restore {}: {e}", path.display());
                }
            }
            if let Some(before) = &self.boundary_before {
                let _ = swap(&self.boundary, before);
            } else {
                let _ = fs::remove_file(&self.boundary);
            }
        }
        if !self.committed
            && let Err(e) = self.write(self.before.as_deref())
        {
            eprintln!("proj: couldn't restore outstanding question: {e}");
        }
    }
}

fn committed_entries(job: &Job, latest: u64) -> Result<Vec<fs::DirEntry>, String> {
    let entries: Vec<_> = fs::read_dir(&job.dir)
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?
        .into_iter()
        // A failed start can leave future prompts or turn artifacts behind.
        // Only the published turn count commits history, for every recovery
        // boundary, source and outbox operation below.
        .filter(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            crate::job::turn_number(&name).is_some_and(|t| t > 0 && t <= latest)
        })
        .collect();
    Ok(entries)
}

/// Only question work needs the full watcher pass. Ordinary jobs retain
/// their existing configured and repository-specific completion kicks.
pub(crate) fn needs_watch(job: &Job) -> bool {
    if job.cfg.notify.is_none() {
        return false;
    }
    let Some(n) = job.turns() else { return false };
    if job.path("open-question").exists()
        || (!job.turn(&n, "stopped").exists()
            && read(&job.turn(&n, "end")).as_deref() == Some("ask"))
    {
        return true;
    }
    let Ok(latest) = n.parse() else { return true };
    committed_entries(job, latest).map_or(true, |entries| {
        entries.iter().any(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.strip_suffix(".question")
                .is_some_and(|t| !job.turn(t, "question-closed").exists())
        })
    })
}

/// Durable per-question outbox. An open is replayed idempotently because the
/// notifier's zero exit is not delivery acknowledgement. A close is forgotten
/// only when `pending` explicitly returns 1. Keys belong to turns, never reused
/// for a later question, even while an older close is still pending.
/// Caller holds the job lock, including during resume rollback.
pub(crate) fn notify(cfg: &crate::config::Config, job: &Job) -> Result<bool, String> {
    use std::process::{Command, Stdio};
    let Some(notifier) = &cfg.notify else {
        return Ok(false);
    };
    let Some(n) = job.turns() else {
        return Ok(false);
    };
    let latest = n
        .parse::<u64>()
        .map_err(|e| format!("invalid committed turn {n}: {e}"))?;
    let stopped = job.turn(&n, "stopped").exists();
    let end = read(&job.turn(&n, "end"));
    let finished = stopped || end.as_deref() == Some("done");
    let marker = read(&job.path("open-question"));
    let answered = read_state(&job.path("question-answered-through"))?
        .and_then(|s| s.trim_end_matches('\n').parse::<u64>().ok())
        .unwrap_or(0);
    let entries = committed_entries(job, latest)?;
    let last_done = entries
        .iter()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let t = name.strip_suffix(".end")?.parse::<u64>().ok()?;
            (read(&e.path()).as_deref() == Some("done")).then_some(t)
        })
        .max()
        .unwrap_or(0);
    // Legacy jobs have no answered-through file. Their saved prompts still
    // distinguish a new conversation turn from our automated continuation.
    let legacy_boundary = entries
        .iter()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let t = name.strip_prefix("prompt.")?.parse::<u64>().ok()?;
            let prompt = read(&e.path())?;
            (!prompt.ends_with(OPEN_UPDATE) && !prompt.ends_with(LEGACY_OPEN_UPDATE))
                .then_some(t.saturating_sub(1))
        })
        .max()
        .unwrap_or(0);
    let closed_through = entries
        .iter()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.strip_suffix(".question-close")
                .or_else(|| name.strip_suffix(".question-closed"))?
                .parse::<u64>()
                .ok()
        })
        .max()
        .unwrap_or(0);
    let boundary = answered
        .max(last_done)
        .max(legacy_boundary)
        .max(closed_through);
    // Recover the FIRST outstanding ask, including when no outbox could be
    // written before automated continuations. Older closes are independent.
    let source = if finished {
        None
    } else {
        marker
            .as_ref()
            .filter(|t| t.parse::<u64>().is_ok_and(|t| t > boundary && t <= latest))
            .cloned()
            .or_else(|| {
                entries
                    .iter()
                    .filter_map(|e| {
                        let name = e.file_name().to_string_lossy().into_owned();
                        let t = name.strip_suffix(".end")?.parse::<u64>().ok()?;
                        (t > boundary && read(&e.path()).as_deref() == Some("ask")).then_some(t)
                    })
                    .min()
                    .map(|t| t.to_string())
            })
    };
    let active = source.clone();
    if let Some(source) =
        source.filter(|t| job.turn(t, "ended").exists() || job.turn(t, "outcome").exists())
    {
        // Keep the body before the marker: either write can fail and the next
        // pass reconstructs the same operation, with the same idempotency key.
        let path = job.turn(&source, "question");
        if !path.exists() {
            let mut body = crate::engine::answer(&job.turn(&source, "jsonl"));
            // Keep argv below Linux's per-argument limit, on a UTF-8 boundary.
            if body.len() > 32_000 {
                let mut at = 32_000;
                while !body.is_char_boundary(at) {
                    at -= 1;
                }
                body.truncate(at);
                body.push_str("\n[Full answer: proj log]");
            }
            if let Some(question) = crate::ask::load(job) {
                body.push_str(&question.notice());
            }
            swap(
                &path,
                &format!(
                    "Job {} needs your answer. Reply with proj reply {}.\n\n{}",
                    job.id, job.id, body
                ),
            )
            .map_err(|e| e.to_string())?;
        }
        swap(&job.path("open-question"), &format!("{source}\n")).map_err(|e| e.to_string())?;
    }
    let mut retry = false;
    let mut errors = Vec::new();
    for entry in committed_entries(job, latest)? {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(turn) = name.strip_suffix(".question") else {
            continue;
        };
        let closing = job.turn(turn, "question-close");
        let closed = job.turn(turn, "question-closed");
        if closed.exists() {
            continue;
        }
        let key = format!("proj-question-{}-{turn}", job.id);
        // A successful human resume clears the marker and advances turns.
        // Failed starts roll both back under the same lock. VM done doesn't
        // clear the marker, so completion is also checked independently.
        if (finished || active.as_deref() != Some(turn))
            && let Err(e) = swap(&closing, "close\n")
        {
            errors.push(e.to_string());
            continue;
        }
        retry = true;
        let result = if closing.exists() {
            Command::new(notifier)
                .args(["close", &key, "The job no longer needs this answer."])
                .status()
        } else {
            let body = match read_state(&entry.path()) {
                Ok(Some(body)) => body,
                Ok(None) => continue,
                Err(e) => {
                    errors.push(e.to_string());
                    continue;
                }
            };
            Command::new(notifier)
                .args(["ensure", &key, "A project job needs an answer", &body])
                .status()
        };
        match result {
            Ok(s) if s.success() => (),
            other => {
                errors.push(format!("question notification: {other:?}"));
                continue;
            }
        }
        if closing.exists()
            && Command::new(notifier)
                .args(["pending", &key])
                .stdout(Stdio::null())
                .status()
                .is_ok_and(|s| s.code() == Some(1))
            && let Err(e) = swap(&closed, "closed\n")
        {
            errors.push(e.to_string());
        }
    }
    if errors.is_empty() {
        Ok(retry)
    } else {
        Err(errors.join("; "))
    }
}

#[cfg(test)]
mod tests;
