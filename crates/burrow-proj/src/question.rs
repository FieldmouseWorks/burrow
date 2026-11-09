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
    close: Option<(PathBuf, String)>,
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
            close: (!asking && (before.is_some() || end.as_deref() == Some("ask")))
                .then(|| {
                    job.cfg
                        .notify
                        .clone()
                        .map(|n| (n, format!("proj-question-{}", job.id)))
                })
                .flatten(),
        };
        state.write(if asking { Some("ask\n") } else { None })?;
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
        if let Some((notifier, key)) = &self.close {
            match std::process::Command::new(notifier)
                .args(["close", key, "The job received an answer."])
                .status()
            {
                Ok(s) if s.success() => (),
                result => eprintln!("proj: closing question notification: {result:?}"),
            }
        }
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

/// Notify once for an open question, using the state preserved across resumes.
pub(crate) fn notify(cfg: &crate::config::Config, job: &Job) -> Result<(), String> {
    let Some(n) = job.turns() else { return Ok(()) };
    if read(&job.turn(&n, "end")).as_deref() != Some("ask") || job.path("open-question").exists() {
        return Ok(());
    }
    if let Some(notifier) = &cfg.notify {
        let question = crate::engine::answer(&job.turn(&n, "jsonl"));
        let status = std::process::Command::new(notifier)
            .args([
                "open",
                &format!("proj-question-{}", job.id),
                "A project job needs an answer",
            ])
            .arg(format!(
                "Job {} needs your answer. Reply with proj reply {}.\n\n{}",
                job.id, job.id, question
            ))
            .status()
            .map_err(|e| e.to_string())?;
        if !status.success() {
            return Err(format!("question notifier exited {status}"));
        }
    }
    swap(&job.path("open-question"), "ask\n").map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        cmd::declaration_tests::response,
        config::{Config, tests::example},
    };

    #[test]
    fn only_new_human_questions_reach_notify() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let mut cfg: Config = serde_json::from_value(example()).unwrap();
        cfg.jobs = dir.path().to_owned();
        let script = dir.path().join("notify");
        let log = dir.path().join("notices");
        fs::write(
            &script,
            format!("#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{}'\n", log.display()),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        cfg.notify = Some(script);
        let job = Job::new(&cfg, "j1");
        fs::create_dir_all(&job.dir).unwrap();
        job.set_turns(1).unwrap();
        for end in ["done", "await", "undeclared"] {
            fs::write(job.turn("1", "end"), end).unwrap();
            notify(&cfg, &job).unwrap();
            assert!(!log.exists());
        }
        fs::write(job.turn("1", "end"), "ask").unwrap();
        fs::write(job.turn("1", "jsonl"), "{\"type\":\"result\",\"result\":\"Which release should we keep?\",\"is_error\":false}\n").unwrap();
        notify(&cfg, &job).unwrap();
        let notice = fs::read_to_string(&log).unwrap();
        assert!(notice.starts_with("open\nproj-question-j1\n"));
        assert!(notice.contains("proj reply j1"));
        assert!(notice.contains("Which release should we keep?"));
        notify(&cfg, &job).unwrap();
        assert_eq!(fs::read_to_string(&log).unwrap(), notice);
        // A machinery resume preserves the question and does not close it.
        Resume::prepare(&job, false).unwrap().commit();
        notify(&cfg, &job).unwrap();
        assert_eq!(fs::read_to_string(&log).unwrap(), notice);
        // Failed starts restore the question and leave its notice open.
        drop(Resume::prepare(&job, true).unwrap());
        assert_eq!(fs::read_to_string(&log).unwrap(), notice);
        Resume::prepare(&job, true).unwrap().commit();
        assert!(
            fs::read_to_string(&log)
                .unwrap()
                .contains("close\nproj-question-j1\n")
        );
        assert!(!job.path("open-question").exists());
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
