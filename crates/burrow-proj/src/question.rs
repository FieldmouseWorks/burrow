//! Outstanding questions belong to the job, across every source of resumes.
//! Prepare a host-owned intent before launching, and retire question state
//! only after a durable acceptance receipt. A new engine must never see yesterday's question as open
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

// The host owns the resume receipt. A prompt and a published turn number are
// preparations, not evidence that systemd accepted the reply. The launcher or
// _exec accepts the receipt; either can finish its idempotent cleanup later.
#[derive(serde::Serialize, serde::Deserialize)]
struct Receipt {
    through: u64,
    answered: bool,
    clear: bool,
    accepted: bool,
    // Reconciled: accepted cleanup applied, or unaccepted intent abandoned.
    applied: bool,
}

const METADATA: &[&str] = &[
    "question",
    "question-needs-owner",
    "question-text",
    "question-since",
    "question-id",
];

fn receipt_path(job: &Job, n: &str) -> PathBuf {
    crate::ask::host_path(job, &format!("resume-{n}"))
}

fn save_receipt(path: &Path, receipt: &Receipt) -> Result<(), String> {
    swap(
        path,
        &serde_json::to_string(receipt).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

fn apply_receipt(path: &Path, job_dir: &Path) -> Result<(), String> {
    let Some(text) = read_state(path)? else {
        return Ok(());
    };
    let mut r: Receipt = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    if !r.accepted || r.applied {
        return Ok(());
    }
    if r.answered {
        crate::state_reads::write(
            job_dir.join("question-answered-through"),
            r.through.to_string(),
        )
        .map_err(|e| e.to_string())?;
    }
    if r.clear {
        for path in METADATA
            .iter()
            .map(|name| path.parent().unwrap().join(name))
            .chain(std::iter::once(job_dir.join("open-question")))
        {
            match crate::state_reads::remove(path) {
                Ok(()) => (),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => return Err(e.to_string()),
            }
        }
    }
    r.applied = true;
    save_receipt(path, &r)
}

/// Caller holds the job lock. Only accepted receipts can retire a question.
pub(crate) fn recover(job: &Job) -> Result<(), String> {
    let dir = crate::ask::host_path(job, "");
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.to_string()),
    };
    let mut receipts = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(n) = name
            .strip_prefix("resume-")
            .and_then(|n| n.parse::<u64>().ok())
        {
            receipts.push((n, entry.path()));
        }
    }
    receipts.sort_by_key(|(n, _)| *n);
    for (n, path) in receipts {
        let Some(text) = read_state(&path)? else {
            continue;
        };
        let mut r: Receipt = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        if r.accepted {
            apply_receipt(&path, &job.dir)?;
        } else if job.turns().as_deref() == Some(n.to_string().as_str()) {
            // The caller owns the launcher lock. A live unit or unknown
            // lifecycle can still accept a preparation; a vanished unit cannot.
            if !r.applied {
                // Read the queued job before the unit state: a queued start
                // may become active between observations. The launcher lock
                // prevents a new launch from racing a known empty queue.
                let pending = crate::systemd::pending(job.cfg, &job.unit());
                let load = crate::systemd::show(job.cfg, &job.unit(), "LoadState");
                let active = crate::systemd::show(job.cfg, &job.unit(), "ActiveState");
                if pending == Some(false)
                    && (load == "not-found" || matches!(active.as_str(), "inactive" | "failed"))
                    && !job.end_pending()
                {
                    // Reconcile as abandoned, never as answered. Keep the
                    // failed turn's identity and prompt without touching any
                    // question authority, its original deadline or its outbox.
                    r.applied = true;
                    save_receipt(&path, &r)?;
                }
            }
            if r.applied
                && !job.turn(&n.to_string(), "stopped").exists()
                && read_state(&job.turn(&n.to_string(), "outcome"))?.is_none()
            {
                crate::state_reads::write(job.turn(&n.to_string(), "outcome"), "failed\n")
                    .map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(())
}

/// Also called at _exec entry: a killed launcher may never return from
/// systemd-run to commit, even though the reply's unit was accepted.
pub(crate) fn accept(job: &Job, n: &str) -> Result<(), String> {
    let path = receipt_path(job, n);
    let Some(text) = read_state(&path)? else {
        return Ok(());
    };
    let mut receipt: Receipt = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    if !receipt.accepted && receipt.applied {
        return Err(format!("prepared turn {n} was abandoned before acceptance"));
    }
    receipt.accepted = true;
    save_receipt(&path, &receipt)?;
    apply_receipt(&path, &job.dir)
}

/// A published prompt whose unit has not recorded acceptance must not let
/// an expired older question execute its default while _exec reconciles.
pub(crate) fn acceptance_pending(job: &Job) -> Result<bool, String> {
    let Some(n) = job.turns() else {
        return Ok(false);
    };
    let Some(text) = read_state(&receipt_path(job, &n))? else {
        return Ok(false);
    };
    let r: Receipt = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    Ok(!r.accepted && !r.applied)
}

pub(crate) struct Resume {
    path: PathBuf,
    before: Option<String>,
    receipt: PathBuf,
    job_dir: PathBuf,
    asking: bool,
    committed: bool,
}

impl Resume {
    pub(crate) fn prepare(job: &Job, answered: bool) -> Result<Self, String> {
        recover(job)?;
        if !answered && crate::ask::host_path(job, "question-needs-owner").exists() {
            return Err("the one-way question is still waiting for a person's answer; machine news cannot resume this completed job".into());
        }
        let path = job.path("open-question");
        let before = read_state(&path)?;
        let end = job.turns().and_then(|n| read(&job.turn(&n, "end")));
        let asking = !answered
            && (crate::ask::load(job).is_some()
                || (end.as_deref() != Some("done")
                    && (before.is_some() || end.as_deref() == Some("ask"))));
        // Validate existing authority before publishing any intent. Leave it
        // untouched until acceptance, so abrupt process death needs no undo.
        read_state(&job.path("question-answered-through"))?;
        for name in METADATA {
            read_state(&crate::ask::host_path(job, name))?;
        }
        crate::ask::ensure(job)?;
        let through = job
            .turns()
            .map(|n| n.parse::<u64>())
            .transpose()
            .map_err(|e| e.to_string())?
            .unwrap_or(0);
        let receipt = receipt_path(job, &(through + 1).to_string());
        save_receipt(
            &receipt,
            &Receipt {
                through,
                answered,
                clear: !asking,
                accepted: false,
                applied: false,
            },
        )?;
        let state = Self {
            path,
            before,
            receipt,
            job_dir: job.dir.clone(),
            asking,
            committed: false,
        };
        if asking {
            state.write(state.before.as_deref().or(Some("ask\n")))?;
        }
        Ok(state)
    }

    pub(crate) fn prompt(&self, msg: &str) -> String {
        if self.asking {
            format!("{msg}\n\n{OPEN_UPDATE}")
        } else {
            msg.to_owned()
        }
    }

    pub(crate) fn commit(mut self) -> Result<(), String> {
        let text = read_state(&self.receipt)?.ok_or("missing resume receipt")?;
        let mut r: Receipt = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        r.accepted = true;
        save_receipt(&self.receipt, &r)?;
        // An accepted receipt must survive even when its cleanup fails.
        self.committed = true;
        apply_receipt(&self.receipt, &self.job_dir)
    }

    fn write(&self, value: Option<&str>) -> Result<(), String> {
        let result = if let Some(value) = value {
            crate::state_reads::write(&self.path, value)
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
        if !self.committed
            && self.asking
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
    recover(job)?;
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
    let pending_owner = crate::ask::host_path(job, "question-needs-owner").exists();
    // A one-way timeout publishes a synthetic done turn to release the job,
    // while its original question still needs the owner's decision.
    let finished = stopped || (end.as_deref() == Some("done") && !pending_owner);
    let marker = read(&job.path("open-question"));
    let answered = read_state(&job.path("question-answered-through"))?
        .and_then(|s| s.trim_end_matches('\n').parse::<u64>().ok())
        .unwrap_or(0);
    let entries = committed_entries(job, latest)?;
    let last_ask = entries
        .iter()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let t = name.strip_suffix(".end")?.parse::<u64>().ok()?;
            (read(&e.path()).as_deref() == Some("ask")).then_some(t)
        })
        .max()
        .unwrap_or(0);
    let last_done = entries
        .iter()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let t = name.strip_suffix(".end")?.parse::<u64>().ok()?;
            // Ignore the timeout's synthetic completion, even after an
            // abandoned human resume publishes a newer failed turn. Earlier
            // completed conversations still bound the outstanding question.
            (read(&e.path()).as_deref() == Some("done") && (!pending_owner || t < last_ask))
                .then_some(t)
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
            // New resumes have host-owned intent records. Preparatory prompts
            // must not acknowledge a reply before the unit accepts it.
            if let Some(text) = read_state(&receipt_path(job, &t.to_string())).ok()? {
                let r: Receipt = serde_json::from_str(&text).ok()?;
                return (r.accepted && r.answered).then_some(r.through);
            }
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
            crate::state_reads::write(
                &path,
                format!(
                    "Job {} needs your answer. Reply with proj reply {}.\n\n{}",
                    job.id, job.id, body
                ),
            )
            .map_err(|e| e.to_string())?;
        }
        crate::state_reads::write(job.path("open-question"), format!("{source}\n"))
            .map_err(|e| e.to_string())?;
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
        // Only an accepted human resume clears the marker. Preparatory
        // writes preserve the question even when the starter dies. VM done doesn't
        // clear the marker, so completion is also checked independently.
        if (finished || active.as_deref() != Some(turn))
            && let Err(e) = crate::state_reads::write(&closing, "close\n")
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
            && let Err(e) = crate::state_reads::write(&closed, "closed\n")
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
