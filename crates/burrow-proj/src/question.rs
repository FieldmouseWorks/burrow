//! Outstanding questions belong to the job, across every source of resumes.
//! Prepare the state before launching an engine, and restore it if the turn
//! cannot start. A new engine must never see yesterday's question as open
//! after a human answered, or lose one because a machinery start failed.

use std::fs;
use std::path::{Path, PathBuf};

use crate::{job::Job, read, swap};

const OPEN_UPDATE: &str = "Your open question is still posted. This update is not a person's reply. Do not repeat the question or ping the owner again. Handle the update, then run `proj ask` again without restating the question if it still needs an answer.";

fn read_state(path: &Path) -> Result<Option<String>, String> {
    match fs::read_to_string(path) {
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
}

impl Resume {
    pub(crate) fn prepare(job: &Job, answered: bool) -> Result<Self, String> {
        let path = job.path("open-question");
        let before = read_state(&path)?;
        let end = job.turns().and_then(|n| read(&job.turn(&n, "end")));
        let asking = !answered
            && end.as_deref() != Some("done")
            && (before.is_some() || end.as_deref() == Some("ask"));
        let boundary = job.path("question-answered-through");
        let boundary_before = read_state(&boundary)?;
        let state = Self {
            boundary,
            boundary_before,
            path,
            before: before.clone(),
            asking,
            committed: false,
        };
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
            let turn = name
                .strip_prefix("prompt.")
                .unwrap_or(&name)
                .split('.')
                .next()
                .unwrap_or_default();
            turn.parse::<u64>().is_ok_and(|t| t > 0 && t <= latest)
        })
        .collect();
    Ok(entries)
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
            (!prompt.ends_with(OPEN_UPDATE)).then_some(t.saturating_sub(1))
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
            let body = match fs::read_to_string(entry.path()) {
                Ok(body) => body,
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
mod tests {
    use super::*;
    use crate::{
        cmd::declaration_tests::response,
        config::{Config, tests::example},
    };

    struct Fixture {
        dir: tempfile::TempDir,
        cfg: Config,
    }
    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let mut cfg: Config = serde_json::from_value(example()).unwrap();
            cfg.jobs = dir.path().join("jobs");
            let script = dir.path().join("notify");
            burrow_test::executable(
                &script,
                format!(
                    r#"#!/bin/sh
cd '{}'
printf '%s %s\n' "$1" "$2" >> calls
case "$1" in
ensure)
  touch "pending-$2"
  test -f fail-open && exit 0
  test -f "remote-$2" || printf '%s\n' "$4" >> deliveries
  touch "remote-$2" ;;
close)
  test -f fail-close && exit 0
  rm -f "remote-$2" "pending-$2" ;;
pending) test -f fail-pending && exit 2
  test -f "pending-$2" ;;
esac
"#,
                    dir.path().display()
                ),
            );
            cfg.notify = Some(script);
            let job = Job::new(&cfg, "j1");
            fs::create_dir_all(&job.dir).unwrap();
            job.set_turns(1).unwrap();
            Self { dir, cfg }
        }
        fn job(&self) -> Job<'_> {
            Job::new(&self.cfg, "j1")
        }
        fn ending(&self, n: u64, how: &str) {
            let j = self.job();
            j.set_turns(n).unwrap();
            fs::write(j.turn(&n.to_string(), "end"), how).unwrap();
            j.mark_ended();
            fs::write(
                j.turn(&n.to_string(), "jsonl"),
                format!(
                    "{{\"type\":\"result\",\"result\":\"Question {n}?\",\"is_error\":false}}\n"
                ),
            )
            .unwrap();
        }
        fn flag(&self, name: &str) {
            fs::write(self.dir.path().join(name), "").unwrap();
        }
        fn clear(&self, name: &str) {
            fs::remove_file(self.dir.path().join(name)).unwrap();
        }
        fn deliveries(&self) -> String {
            fs::read_to_string(self.dir.path().join("deliveries")).unwrap_or_default()
        }
    }

    #[test]
    fn notifications_disabled_do_not_add_watcher_kicks() {
        use std::sync::{Arc, Mutex};
        let mut f = Fixture::new();
        f.cfg.notify = None;
        f.cfg.waits = None;
        let got = Arc::new(Mutex::new(Vec::new()));
        let seen = got.clone();
        let (port, _) = burrow_events::http::listen(
            0,
            Arc::new(move |req| {
                seen.lock().unwrap().push(req.target.clone());
                202
            }),
        )
        .unwrap();
        f.cfg.kick = format!("http://127.0.0.1:{port}/kick");
        f.cfg.tools.systemctl = "false".into();
        f.cfg.tools.systemd_run = "true".into();
        f.ending(1, "ask");
        crate::turn::start_answer(&f.cfg, "j1", "answer", false).unwrap();
        f.ending(2, "ask");
        fs::write(f.job().path("kick"), "completion").unwrap();
        crate::cmd::finish(&f.cfg, "j1").unwrap();
        f.cfg.tools.systemctl = "true".into();
        crate::cmd::stop(&f.cfg, "j1", false).unwrap();
        assert_eq!(*got.lock().unwrap(), ["/kick/completion"]);
    }

    #[test]
    fn finalization_defers_instead_of_waiting_on_resume_lock() {
        use std::sync::{Arc, Mutex, mpsc};
        use std::time::Duration;
        let mut f = Fixture::new();
        f.cfg.wake = f.dir.path().join("wake.json");
        let got = Arc::new(Mutex::new(Vec::new()));
        let seen = got.clone();
        let (port, _) = burrow_events::http::listen(
            0,
            Arc::new(move |req| {
                seen.lock().unwrap().push(req.target.clone());
                202
            }),
        )
        .unwrap();
        f.cfg.kick = format!("http://127.0.0.1:{port}/kick");
        f.cfg.waits = None;
        f.ending(1, "ask");
        let held = f.job().lock().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                tx.send(crate::cmd::finish(&f.cfg, "j1").is_ok()).unwrap();
            });
            let completed = rx.recv_timeout(Duration::from_secs(5));
            drop(held);
            assert!(completed.unwrap());
        });
        assert!(got.lock().unwrap().iter().any(|s| s == "/kick/proj-watch"));
        assert!(f.deliveries().is_empty());
        crate::watch::look(&f.cfg, "").unwrap();
        assert!(f.deliveries().contains("Question 1?"));
    }

    #[test]
    fn newer_marker_cannot_erase_an_older_failed_close_write() {
        let f = Fixture::new();
        f.ending(1, "ask");
        notify(&f.cfg, &f.job()).unwrap();
        Resume::prepare(&f.job(), true).unwrap().commit();
        f.ending(2, "ask");
        fs::create_dir(f.job().turn("1", "question-close")).unwrap();
        assert!(notify(&f.cfg, &f.job()).is_err());
        assert_eq!(read(&f.job().path("open-question")).as_deref(), Some("2"));
        fs::remove_dir(f.job().turn("1", "question-close")).unwrap();
        notify(&f.cfg, &f.job()).unwrap();
        assert!(f.job().turn("1", "question-closed").exists());
        assert_eq!(f.deliveries().matches("Question 1?").count(), 1);
        assert!(f.dir.path().join("remote-proj-question-j1-2").exists());
    }

    #[test]
    fn recovery_keeps_origin_across_automated_updates_and_unrelated_closes() {
        for legacy in [false, true] {
            let f = Fixture::new();
            f.ending(1, "ask");
            notify(&f.cfg, &f.job()).unwrap();
            f.flag("fail-close");
            Resume::prepare(&f.job(), true).unwrap().commit();
            f.ending(2, "ask");
            fs::write(f.job().turn("1", "question-close"), "close").unwrap();
            fs::remove_file(f.job().path("question-answered-through")).unwrap();
            if legacy {
                fs::write(f.job().path("open-question"), "ask").unwrap();
            } else {
                fs::create_dir(f.job().turn("2", "question.new")).unwrap();
                assert!(notify(&f.cfg, &f.job()).is_err());
                fs::remove_dir(f.job().turn("2", "question.new")).unwrap();
            }
            Resume::prepare(&f.job(), false).unwrap().commit();
            f.ending(3, "ask");
            notify(&f.cfg, &f.job()).unwrap();
            assert!(f.deliveries().contains("Question 2?"));
            assert!(!f.deliveries().contains("Question 3?"));
            assert_eq!(read(&f.job().path("open-question")).as_deref(), Some("2"));
        }
    }

    #[test]
    fn legacy_recovery_excludes_previously_answered_questions() {
        let f = Fixture::new();
        f.ending(1, "ask");
        f.ending(2, "ask");
        fs::write(
            f.job().path("prompt.2"),
            "A human answers the earlier question",
        )
        .unwrap();
        fs::write(
            f.job().path("prompt.3"),
            format!("event\n\n{OPEN_UPDATE}\n"),
        )
        .unwrap();
        fs::write(f.job().path("open-question"), "ask").unwrap();
        f.ending(3, "ask");
        notify(&f.cfg, &f.job()).unwrap();
        assert!(f.deliveries().contains("Question 2?"));
        assert!(!f.deliveries().contains("Question 1?"));
        assert!(!f.deliveries().contains("Question 3?"));
    }

    #[test]
    fn orphaned_future_turns_cannot_answer_complete_or_replace_a_question() {
        for artifact in ["prompt", "done", "close", "closed", "replacement"] {
            for recorded in [false, true] {
                let f = Fixture::new();
                f.ending(1, "ask");
                if recorded {
                    notify(&f.cfg, &f.job()).unwrap();
                }
                fs::write(f.job().path("open-question"), "ask").unwrap();
                match artifact {
                    "prompt" => {
                        fs::write(f.job().path("prompt.2"), "partial human answer").unwrap()
                    }
                    "done" => fs::write(f.job().turn("2", "end"), "done").unwrap(),
                    "close" => fs::write(f.job().turn("2", "question-close"), "close").unwrap(),
                    "closed" => fs::write(f.job().turn("2", "question-closed"), "closed").unwrap(),
                    _ => {
                        fs::write(f.job().turn("2", "end"), "ask").unwrap();
                        fs::write(f.job().turn("2", "ended"), "").unwrap();
                        fs::write(f.job().turn("2", "question"), "uncommitted question").unwrap();
                        fs::write(f.job().path("open-question"), "2").unwrap();
                    }
                }
                notify(&f.cfg, &f.job()).unwrap();
                assert!(
                    f.deliveries().contains("Question 1?"),
                    "{artifact}, recorded={recorded}"
                );
                assert!(!f.deliveries().contains("uncommitted"));
                assert!(!f.job().turn("1", "question-close").exists());
                assert_eq!(
                    f.job().turn("2", "question-close").exists(),
                    artifact == "close"
                );
                assert_eq!(read(&f.job().path("open-question")).as_deref(), Some("1"));
            }
        }
    }

    #[test]
    fn uncommitted_ask_cannot_create_a_question() {
        let f = Fixture::new();
        f.ending(1, "await");
        fs::write(f.job().turn("2", "end"), "ask").unwrap();
        fs::write(f.job().turn("2", "ended"), "").unwrap();
        fs::write(f.job().turn("2", "question"), "uncommitted question").unwrap();
        fs::write(f.job().path("open-question"), "2").unwrap();
        assert!(!notify(&f.cfg, &f.job()).unwrap());
        assert!(f.deliveries().is_empty());
    }

    #[test]
    fn failed_prompt_writes_preserve_questions_for_human_and_automated_starts() {
        for human in [false, true] {
            let mut f = Fixture::new();
            f.cfg.tools.systemctl = "false".into();
            f.cfg.tools.systemd_run = "false".into();
            f.cfg.kick.clear();
            f.cfg.waits = None;
            f.ending(1, "ask");
            notify(&f.cfg, &f.job()).unwrap();
            let prompt = f.job().path("prompt.2");
            // A pre-existing orphan whose write fails before the new start.
            // The cleanup also covers partial/truncated regular-file writes.
            std::os::unix::fs::symlink(f.dir.path().join("missing/prompt"), &prompt).unwrap();
            let result = if human {
                crate::turn::start_answer(&f.cfg, "j1", "answer", false)
            } else {
                crate::turn::start(&f.cfg, "j1", "event", false)
            };
            assert!(result.unwrap_err().contains("prompt.2"));
            assert!(fs::symlink_metadata(&prompt).is_err());
            assert_eq!(f.job().turns().as_deref(), Some("1"));
            assert_eq!(read(&f.job().path("open-question")).as_deref(), Some("1"));
            assert!(!f.job().path("question-answered-through").exists());
            notify(&f.cfg, &f.job()).unwrap();
            assert!(!f.job().turn("1", "question-close").exists());
            assert_eq!(f.deliveries().matches("Question 1?").count(), 1);
        }
    }

    #[test]
    fn failed_answer_start_restores_closure_boundary_atomically() {
        let f = Fixture::new();
        f.ending(1, "ask");
        notify(&f.cfg, &f.job()).unwrap();
        fs::write(f.job().path("question-answered-through"), [0xff]).unwrap();
        assert!(Resume::prepare(&f.job(), true).is_err());
        assert_eq!(
            fs::read(f.job().path("question-answered-through")).unwrap(),
            [0xff]
        );
        assert!(notify(&f.cfg, &f.job()).is_err());
        assert!(f.job().path("open-question").exists());
        fs::remove_file(f.job().path("question-answered-through")).unwrap();
        fs::create_dir(f.job().path("question-answered-through.new")).unwrap();
        assert!(Resume::prepare(&f.job(), true).is_err());
        fs::remove_dir(f.job().path("question-answered-through.new")).unwrap();
        assert!(f.job().path("open-question").exists());
        drop(Resume::prepare(&f.job(), true).unwrap());
        assert!(!f.job().path("question-answered-through").exists());
        notify(&f.cfg, &f.job()).unwrap();
        assert!(!f.job().turn("1", "question-close").exists());
    }

    #[test]
    fn only_new_human_questions_reach_notify() {
        let f = Fixture::new();
        let j = f.job();
        for end in ["done", "await", "undeclared"] {
            f.ending(1, end);
            assert!(!notify(&f.cfg, &j).unwrap());
            assert!(f.deliveries().is_empty());
        }
        f.ending(1, "ask");
        notify(&f.cfg, &j).unwrap();
        assert!(f.deliveries().contains("Question 1?"));
        let notice = f.deliveries();
        notify(&f.cfg, &j).unwrap();
        Resume::prepare(&j, false).unwrap().commit();
        notify(&f.cfg, &j).unwrap();
        assert_eq!(f.deliveries(), notice);
        drop(Resume::prepare(&j, true).unwrap());
        notify(&f.cfg, &j).unwrap();
        assert_eq!(f.deliveries(), notice);
        Resume::prepare(&j, true).unwrap().commit();
        j.set_turns(2).unwrap();
        notify(&f.cfg, &j).unwrap();
        assert!(j.turn("1", "question-closed").exists());
    }

    #[test]
    fn ambiguous_delivery_and_closure_survive_restarts_and_later_questions() {
        let f = Fixture::new();
        f.ending(1, "ask");
        f.flag("fail-open");
        notify(&f.cfg, &f.job()).unwrap(); // zero exit, but no delivery
        assert!(f.deliveries().is_empty());
        f.clear("fail-open");
        // State is on disk: a fresh Job retries the ambiguous operation.
        notify(&f.cfg, &f.job()).unwrap();
        notify(&f.cfg, &f.job()).unwrap();
        assert_eq!(f.deliveries().matches("Question 1?").count(), 1);
        f.flag("fail-close");
        Resume::prepare(&f.job(), true).unwrap().commit();
        f.ending(2, "ask");
        notify(&f.cfg, &f.job()).unwrap();
        assert!(f.job().turn("1", "question-close").exists());
        assert!(!f.job().turn("1", "question-closed").exists());
        assert!(f.deliveries().contains("Question 2?"));
        f.clear("fail-close");
        notify(&f.cfg, &f.job()).unwrap();
        assert!(f.job().turn("1", "question-closed").exists());
        assert!(f.dir.path().join("remote-proj-question-j1-2").exists());
        assert_eq!(f.deliveries().matches("Question 2?").count(), 1);
    }

    #[test]
    fn done_and_stopped_jobs_close_notices_without_the_question_marker() {
        for stopped in [false, true] {
            for native in [false, true] {
                let f = Fixture::new();
                f.ending(1, "ask");
                notify(&f.cfg, &f.job()).unwrap();
                f.ending(2, if stopped { "ask" } else { "done" });
                if stopped {
                    fs::write(f.job().turn("2", "stopped"), "").unwrap();
                }
                if native {
                    f.clear("jobs/j1/open-question");
                }
                notify(&f.cfg, &f.job()).unwrap();
                assert!(f.job().turn("1", "question-closed").exists());
                assert!(!f.job().turn("2", "question").exists());
                assert!(!notify(&f.cfg, &f.job()).unwrap());
            }
        }
    }

    #[test]
    fn launch_nonzero_and_state_write_errors_retain_work() {
        let mut f = Fixture::new();
        f.ending(1, "ask");
        let notifier = f.cfg.notify.clone();
        f.cfg.notify = Some(f.dir.path().join("missing"));
        assert!(notify(&f.cfg, &f.job()).is_err());
        assert!(f.job().turn("1", "question").exists());
        f.cfg.notify = Some("false".into());
        assert!(notify(&f.cfg, &f.job()).is_err());
        f.cfg.notify = notifier;
        notify(&f.cfg, &f.job()).unwrap();
        assert!(f.deliveries().contains("Question 1?"));
        f.ending(2, "done");
        fs::create_dir(f.job().turn("1", "question-close")).unwrap();
        assert!(notify(&f.cfg, &f.job()).is_err());
        fs::remove_dir(f.job().turn("1", "question-close")).unwrap();
        notify(&f.cfg, &f.job()).unwrap();
        assert!(f.job().turn("1", "question-closed").exists());
    }

    #[test]
    fn ambiguous_pending_and_large_answers_stay_recoverable() {
        let f = Fixture::new();
        f.ending(1, "ask");
        let stream =
            serde_json::json!({"type":"result", "result":"é".repeat(100_000), "is_error":false});
        fs::write(f.job().turn("1", "jsonl"), format!("{stream}\n")).unwrap();
        notify(&f.cfg, &f.job()).unwrap();
        assert!(f.deliveries().contains("Full answer: proj log"));
        assert!(f.deliveries().len() < 33_000);
        f.ending(2, "done");
        f.flag("fail-pending");
        notify(&f.cfg, &f.job()).unwrap();
        assert!(!f.job().turn("1", "question-closed").exists());
        f.clear("fail-pending");
        notify(&f.cfg, &f.job()).unwrap();
        assert!(f.job().turn("1", "question-closed").exists());
    }

    #[test]
    fn finalization_runs_every_obligation_despite_notification_errors() {
        use std::sync::{Arc, Mutex};
        for failure in ["launch", "nonzero", "state"] {
            let mut f = Fixture::new();
            f.cfg.scratch = f.dir.path().join("scratch");
            f.cfg.waits = Some(f.dir.path().join("waits.json"));
            f.cfg.tools.systemctl = "false".into();
            let inbox = f.cfg.inbox.as_mut().unwrap();
            inbox.state = f.dir.path().join("inbox");
            fs::create_dir_all(&inbox.state).unwrap();
            fs::write(inbox.state.join(crate::inbox::WAITING), "sam/repo").unwrap();
            f.ending(1, "ask");
            fs::write(f.job().path("owned"), "").unwrap();
            fs::write(f.job().path("kick"), "completion").unwrap();
            let scratch = crate::scratch::dir(&f.cfg, "j1");
            fs::create_dir_all(&scratch).unwrap();
            fs::write(scratch.join("artifact"), "work").unwrap();
            match failure {
                "launch" => f.cfg.notify = Some(f.dir.path().join("absent")),
                "nonzero" => f.cfg.notify = Some("false".into()),
                _ => fs::create_dir(f.job().turn("1", "question")).unwrap(),
            }
            let got = Arc::new(Mutex::new(Vec::new()));
            let seen = got.clone();
            let (port, _) = burrow_events::http::listen(
                0,
                Arc::new(move |req| {
                    seen.lock().unwrap().push(req.target.clone());
                    202
                }),
            )
            .unwrap();
            f.cfg.kick = format!("http://127.0.0.1:{port}/kick");
            assert!(crate::cmd::finish(&f.cfg, "j1").is_err());
            assert!(f.job().turn("1", "ended").exists());
            assert!(f.cfg.waits.as_ref().unwrap().exists());
            assert!(!scratch.exists());
            let calls = got.lock().unwrap();
            for expected in [
                "/kick/completion",
                "/kick/proj-inbox/sam/repo",
                "/kick/proj-watch",
            ] {
                assert!(calls.iter().any(|s| s == expected), "{failure}: {calls:?}");
            }
        }
    }

    #[test]
    fn watcher_retries_notices_without_github_waits_and_stop_closes_them() {
        let mut f = Fixture::new();
        f.cfg.kick.clear();
        f.cfg.waits = None;
        f.cfg.wake = f.dir.path().join("wake.json");
        f.cfg.tools.systemctl = "false".into();
        f.ending(1, "ask");
        f.flag("fail-open");
        crate::watch::look(&f.cfg, "").unwrap();
        assert!(f.deliveries().is_empty());
        let wake: serde_json::Value =
            serde_json::from_slice(&fs::read(&f.cfg.wake).unwrap()).unwrap();
        assert!(wake[""].as_i64().unwrap() > crate::now());
        f.clear("fail-open");
        crate::watch::look(&f.cfg, "").unwrap();
        assert!(f.deliveries().contains("Question 1?"));
        // A successful explicit stop goes through the same durable close.
        f.cfg.tools.systemctl = "true".into();
        crate::cmd::stop(&f.cfg, "j1", false).unwrap();
        assert!(f.job().turn("1", "question-closed").exists());
        crate::watch::look(&f.cfg, "").unwrap();
        assert_eq!(f.deliveries().matches("Question 1?").count(), 1);
    }

    #[test]
    fn legacy_open_marker_does_not_acknowledge_delivery() {
        let f = Fixture::new();
        f.ending(1, "ask");
        Resume::prepare(&f.job(), false).unwrap().commit();
        assert!(f.job().path("open-question").exists());
        assert!(!f.job().turn("1", "question").exists());
        f.job().set_turns(2).unwrap();
        notify(&f.cfg, &f.job()).unwrap();
        assert!(f.deliveries().contains("Question 1?"));
        notify(&f.cfg, &f.job()).unwrap();
        assert_eq!(f.deliveries().matches("Question 1?").count(), 1);
    }

    #[test]
    fn initial_and_completed_questions_keep_normal_declarations() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg: Config = serde_json::from_value(example()).unwrap();
        cfg.jobs = dir.path().to_owned();
        let job = Job::new(&cfg, "j1");
        fs::create_dir_all(&job.dir).unwrap();
        let initial = Resume::prepare(&job, false).unwrap();
        assert_eq!(initial.prompt("new task"), "new task");
        initial.commit();
        assert!(response(&cfg, "j1", "ask").contains("with your question"));
        job.set_turns(1).unwrap();
        fs::write(job.turn("1", "end"), "ask\n").unwrap();
        // A legacy asking job has no separate marker until its next turn.
        let retry = Resume::prepare(&job, false).unwrap();
        assert!(retry.prompt("update").contains("Do not repeat"));
        drop(retry);
        assert!(!job.path("open-question").exists());
        Resume::prepare(&job, false).unwrap().commit();
        assert!(response(&cfg, "j1", "ask").contains("without repeating"));
        response(&cfg, "j1", "done");
        assert!(!job.path("open-question").exists());
        fs::write(job.turn("1", "end"), "done\n").unwrap();
        let next = Resume::prepare(&job, false).unwrap();
        assert_eq!(next.prompt("new task"), "new task");
        next.commit();
        assert!(response(&cfg, "j1", "ask").contains("with your question"));
    }
}
