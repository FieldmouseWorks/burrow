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
            before,
            asking,
            committed: false,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        cmd::declaration_tests::response,
        config::{Config, tests::example},
    };

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
