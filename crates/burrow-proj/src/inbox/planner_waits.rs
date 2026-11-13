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

    // Called only with a successful listing, never after an outage; a scoped
    // pass retires only the waits of issues it asked for.
    pub(super) fn reconcile_planner_waits(&self, repo: &str, all: &[Value]) -> Result<(), Fail> {
        for (kept, n, path) in self.planner_wait_records() {
            if key(repo).eq_ignore_ascii_case(&kept)
                && self.in_scope(&n)
                && !all.iter().any(|i| raw(&i["number"]) == n)
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
        let repo_key = key(repo).to_lowercase();
        let f = dir.join(format!("{repo_key}-{n}"));
        let variants: Vec<_> = self
            .planner_wait_records()
            .into_iter()
            .filter(|(kept, number, _)| kept == &repo_key && number == n)
            .map(|(_, _, path)| path)
            .collect();
        match why {
            Some(why) => {
                let line = format!("{repo}#{n}: {why}");
                if read(&f).as_deref() != Some(line.as_str()) {
                    eprintln!("proj-inbox: {line}");
                    if fs::create_dir_all(&dir).is_err() || fs::write(&f, &line).is_err() {
                        return;
                    }
                }
                // Remove legacy spellings only after the replacement exists.
                for path in variants.into_iter().filter(|path| path != &f) {
                    let _ = fs::remove_file(path);
                }
            }
            None => {
                if !variants.is_empty() {
                    eprintln!("proj-inbox: {repo}#{n}: no longer waiting for sorting");
                    for path in variants {
                        let _ = fs::remove_file(path);
                    }
                }
            }
        }
    }
}
