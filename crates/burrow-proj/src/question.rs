//! Outstanding questions belong to the job, across every source of resumes.
//! Prepare the state before launching an engine, and restore it if the turn
//! cannot start. A new engine must never see yesterday's question as open
//! after a human answered, or lose one because a machinery start failed.

use std::fs;
use std::path::PathBuf;

use crate::{job::Job, read, swap};

pub(crate) struct Resume {
    path: PathBuf,
    before: Option<String>,
    asking: bool,
    committed: bool,
}

impl Resume {
    pub(crate) fn prepare(job: &Job, answered: bool) -> Result<Self, String> {
        let path = job.path("open-question");
        let before = match fs::read_to_string(&path) {
            Ok(s) => Some(s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let end = job.turns().and_then(|n| read(&job.turn(&n, "end")));
        let asking = !answered
            && end.as_deref() != Some("done")
            && (before.is_some() || end.as_deref() == Some("ask"));
        let state = Self {
            path,
            before: before.clone(),
            asking,
            committed: false,
        };
        state.write(if asking {
            state.before.as_deref().or(Some("ask\n"))
        } else {
            None
        })?;
        Ok(state)
    }

    pub(crate) fn prompt(&self, msg: &str) -> String {
        if self.asking {
            format!(
                "{msg}\n\nYour open question is still posted. This update is not a person's reply. Do not repeat the question or ping the owner again. Handle the update, then run `proj ask` again without restating the question if it still needs an answer."
            )
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
        if !self.committed
            && let Err(e) = self.write(self.before.as_deref())
        {
            eprintln!("proj: couldn't restore outstanding question: {e}");
        }
    }
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
    let stopped = job.turn(&n, "stopped").exists();
    let end = read(&job.turn(&n, "end"));
    let finished = stopped || end.as_deref() == Some("done");
    let marker = read(&job.path("open-question"));
    let was_open = marker.is_some();
    let entries: Vec<_> = fs::read_dir(&job.dir)
        .map_err(|e| e.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    let recorded = entries.iter().any(|e| {
        let name = e.file_name().to_string_lossy().into_owned();
        name.strip_suffix(".question")
            .is_some_and(|t| !job.turn(t, "question-closed").exists())
    });
    // Migrate legacy #549 markers too: an outstanding question is not proof
    // that its notification was delivered. Recover its source from turn logs.
    let source = if finished {
        None
    } else if marker.as_deref() == Some("ask") && !recorded {
        entries
            .iter()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let t = name.strip_suffix(".end")?;
                (read(&e.path()).as_deref() == Some("ask"))
                    .then(|| t.parse::<u64>().ok())
                    .flatten()
            })
            .max()
            .map(|t| t.to_string())
    } else if let Some(t) = &marker {
        (!job.turn(t, "question").exists() && t.bytes().all(|b| b.is_ascii_digit()))
            .then(|| t.clone())
    } else {
        (end.as_deref() == Some("ask")).then(|| n.clone())
    };
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
    for entry in fs::read_dir(&job.dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
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
        if finished || (!was_open && turn != n) {
            if let Err(e) = swap(&closing, "close\n") {
                errors.push(e.to_string());
                continue;
            }
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
                .args(["open", &key, "A project job needs an answer", &body])
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
        {
            if let Err(e) = swap(&closed, "closed\n") {
                errors.push(e.to_string());
            }
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
            use std::os::unix::fs::PermissionsExt;
            let dir = tempfile::tempdir().unwrap();
            let mut cfg: Config = serde_json::from_value(example()).unwrap();
            cfg.jobs = dir.path().join("jobs");
            let script = dir.path().join("notify");
            fs::write(
                &script,
                format!(
                    r#"#!/bin/sh
cd '{}'
printf '%s %s\n' "$1" "$2" >> calls
case "$1" in
open)
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
            )
            .unwrap();
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
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
