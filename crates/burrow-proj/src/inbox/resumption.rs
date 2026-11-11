//! A parked handoff reacquires the same shared admission capacity as a new
//! one. The inbox pass lock serializes both; off operations never take it.
use super::{Pass, dirs};
use crate::{config::Config, job::Job, read};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions, TryLockError},
    path::Path,
};

pub(crate) enum Resume {
    Ready(Option<File>),
    Waiting,
}

pub(crate) fn admission(cfg: &Config, job: &Job) -> Result<Resume, String> {
    let Some(inbox) = &cfg.inbox else {
        return Ok(Resume::Ready(None));
    };
    let lock = OpenOptions::new()
        .append(true)
        .create(true)
        .open(format!("{}.lock", inbox.wake.display()))
        .map_err(|e| e.to_string())?;
    match lock.try_lock() {
        Ok(()) => (),
        Err(TryLockError::WouldBlock) => return Ok(Resume::Waiting),
        Err(e) => return Err(e.to_string()),
    }
    let Some(t) = &inbox.triage else {
        return Ok(Resume::Ready(Some(lock)));
    };
    let pass = Pass {
        cfg,
        inbox,
        project: BTreeMap::new(),
        wake: BTreeMap::new(),
        unreached: Vec::new(),
        looked: Vec::new(),
    };
    let mine = |d: &Path| read(&d.join("job")).as_deref() == Some(job.id);
    let drained = dirs(&inbox.state, "");
    if drained
        .iter()
        .any(|d| mine(d) && d.join("drained").exists())
        && !job.path("unstick").exists()
        && drained
            .iter()
            .filter(|d| !mine(d) && pass.draining_record(d))
            .count()
            >= t.drain_jobs
    {
        return Ok(Resume::Waiting);
    }
    let sorting = dirs(&inbox.state.join(".triage"), "");
    if sorting.iter().any(|d| mine(d))
        && !job.path("unstick").exists()
        && sorting
            .iter()
            .filter(|d| !mine(d) && pass.sorting_slot(d))
            .count()
            >= t.at_once
    {
        return Ok(Resume::Waiting);
    }
    Ok(Resume::Ready(Some(lock)))
}

impl Pass<'_> {
    /// Shared occupancy predicate. A stale pending handoff is conservative
    /// until the normal pass can reconcile it with GitHub.
    pub(super) fn draining_record(&self, d: &Path) -> bool {
        if !d.join("drained").exists() || !self.record_uses_capacity(d) {
            return false;
        }
        let named = read(&d.join("drained")).unwrap_or_default();
        !named
            .trim()
            .split_once('#')
            .is_some_and(|(repo, n)| self.inbox.planning && self.has_task_list(Some(repo), n))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inbox::sorting_tests::Box;
    use crate::policy::{self, Identity, Kind};
    use std::fs;

    #[test]
    fn restored_workers_reacquire_serialized_drain_and_sorting_capacity() {
        for sorting in [false, true] {
            let mut fixture = Box::new();
            fixture.cfg.inbox.as_mut().unwrap().wake = fixture.path("wake.json");
            fixture
                .cfg
                .inbox
                .as_mut()
                .unwrap()
                .triage
                .as_mut()
                .unwrap()
                .at_once = 1;
            for id in ["parked", "other"] {
                fixture.job_ending(id, "done", Some("ask"), "kept question");
                let job = Job::new(&fixture.cfg, id);
                policy::bind(
                    &job,
                    Identity::Repository("sam/box".into()),
                    if sorting {
                        Kind::Sorting
                    } else {
                        Kind::Automated
                    },
                )
                .unwrap();
                let root = &fixture.cfg.inbox.as_ref().unwrap().state;
                let dir = if sorting {
                    root.join(".triage").join(id)
                } else {
                    root.join(id)
                };
                fs::create_dir_all(&dir).unwrap();
                fs::write(dir.join("job"), id).unwrap();
                if !sorting {
                    fs::write(dir.join("drained"), "sam/box#1").unwrap();
                }
            }
            let job = Job::new(&fixture.cfg, "parked");
            policy::mark(&job, "done", false).unwrap();
            assert!(matches!(
                admission(&fixture.cfg, &job).unwrap(),
                Resume::Waiting
            ));
            assert!(policy::parked(&job));
            // An outstanding question still consumes the other worker's lane.
            let other = Job::new(&fixture.cfg, "other");
            policy::mark(&other, "done", false).unwrap();
            let Resume::Ready(_guard) = admission(&fixture.cfg, &job).unwrap() else {
                panic!("freed lane was not available")
            };
            assert!(
                matches!(admission(&fixture.cfg, &other).unwrap(), Resume::Waiting),
                "serialized with both new and restored work"
            );
        }
    }
}
