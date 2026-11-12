//! Persist issues awaiting sorting admission or verdict application. Successful issue lists
//! retire vanished issues; project discovery retires removed repositories.
use super::{Pass, key, raw};
use crate::{Fail, io_fail, read};
use serde_json::Value;
use std::{fs, path::PathBuf};

impl Pass<'_> {
    fn planner_wait_records(&self) -> Vec<(String, String, PathBuf)> {
        fs::read_dir(self.inbox.state.join(".triage-waiting"))
            .into_iter()
            .flatten()
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let name = entry.file_name().to_string_lossy().into_owned();
                let (repo, n) = name.rsplit_once('-')?;
                Some((repo.to_lowercase(), n.to_owned(), entry.path()))
            })
            .collect()
    }

    pub(super) fn prune_planner_waits(&self) -> Result<(), Fail> {
        for (repo, _, path) in self.planner_wait_records() {
            if !self
                .project
                .keys()
                .any(|r| key(r).eq_ignore_ascii_case(&repo))
            {
                fs::remove_file(&path).map_err(|e| io_fail(&path, e))?;
            }
        }
        Ok(())
    }

    // Called only with a successful, complete listing, never after an outage.
    pub(super) fn reconcile_planner_waits(&self, repo: &str, all: &[Value]) -> Result<(), Fail> {
        for (kept, n, path) in self.planner_wait_records() {
            if key(repo).eq_ignore_ascii_case(&kept) && !all.iter().any(|i| raw(&i["number"]) == n)
            {
                fs::remove_file(&path).map_err(|e| io_fail(&path, e))?;
            }
        }
        Ok(())
    }

    /// Records, once per change, that issue N waits (or no longer waits) for
    /// sorting admission or verdict application: in the journal, and in `.triage-waiting/KEY-N` for the
    /// snapshot.
    pub(super) fn planner_wait(&self, repo: &str, n: &str, why: Option<&str>) {
        let dir = self.inbox.state.join(".triage-waiting");
        let f = dir.join(format!("{}-{n}", key(repo)));
        let had = read(&f);
        match why {
            Some(why) => {
                let line = format!("{repo}#{n}: {why}");
                if had.as_deref() != Some(line.as_str()) {
                    eprintln!("proj-inbox: {line}");
                    if fs::create_dir_all(&dir).is_ok() {
                        let _ = fs::write(&f, &line);
                    }
                }
            }
            None => {
                if had.is_some() {
                    eprintln!("proj-inbox: {repo}#{n}: no longer waiting for sorting");
                    let _ = fs::remove_file(&f);
                }
            }
        }
    }
}
