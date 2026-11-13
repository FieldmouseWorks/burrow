//! A parked handoff reacquires the same shared admission capacity as a new
//! one. The inbox pass lock serializes both; off operations never take it.
use super::{Pass, dirs};
use crate::{config::Config, job::Job, read};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions, TryLockError},
    path::Path,
};

pub(crate) struct Permit {
    _guard: Option<File>,
}

impl Permit {
    pub(super) fn new(lock: File) -> Self {
        Self { _guard: Some(lock) }
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        // Closing alone leaves the lock held while a concurrently forked
        // child has the same open file description, until that child execs.
        if let Some(lock) = &self._guard {
            let _ = lock.unlock();
        }
    }
}

pub(crate) enum Resume {
    Ready(Permit),
    Waiting,
}

pub(crate) fn admission(cfg: &Config, job: &Job) -> Result<Resume, String> {
    let Some(inbox) = &cfg.inbox else {
        return Ok(Resume::Ready(Permit { _guard: None }));
    };
    let mine = |d: &Path| read(&d.join("job")).as_deref() == Some(job.id);
    let associated = dirs(&inbox.state, "")
        .iter()
        .any(|d| mine(d) && d.join("drained").exists())
        || dirs(&inbox.state.join(".triage"), "")
            .iter()
            .any(|d| mine(d));
    if !associated || inbox.triage.is_none() || job.path("unstick").exists() {
        return Ok(Resume::Ready(Permit { _guard: None }));
    }
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
    let permit = Permit::new(lock);
    let Some(t) = &inbox.triage else {
        return Ok(Resume::Ready(permit));
    };
    let pass = Pass {
        cfg,
        inbox,
        project: BTreeMap::new(),
        wake: BTreeMap::new(),
        unreached: Vec::new(),
        looked: Vec::new(),
        observe: false,
        scope: None,
        drain_at: None,
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
    Ok(Resume::Ready(permit))
}

impl Pass<'_> {
    /// A paused record retains its results, but only an active turn consumes
    /// shared admission capacity. Unstarted handoffs cannot start while paused.
    pub(super) fn record_uses_capacity(&self, dir: &Path) -> bool {
        let name = dir.file_name().unwrap_or_default().to_string_lossy();
        let paused = name
            .rsplit_once('-')
            .is_some_and(|(repo, _)| self.cfg.paused(&repo.replacen('_', "/", 1)));
        if let Some(id) = read(&dir.join("job")) {
            let job = Job::new(self.cfg, &id);
            if !job.dir.is_dir() {
                return !paused;
            }
            if crate::policy::parked(&job) {
                return !crate::systemd::gone(self.cfg, &job.unit());
            }
            if crate::policy::denied(&job) {
                return job.state() == "running";
            }
            return true;
        }
        !paused
    }

    /// Shared occupancy predicate. A stale pending handoff is conservative
    /// until the normal pass can reconcile it with GitHub.
    pub(super) fn draining_record(&self, d: &Path) -> bool {
        self.drain_user(d).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inbox::sorting_tests::Box;
    use crate::policy::{self, Identity, Kind};
    use std::fs;

    #[test]
    fn a_released_permit_does_not_wait_for_an_inherited_descriptor() {
        let fixture = Box::new();
        let lock = fixture.path("wake.json.lock");
        let file = File::create(&lock).unwrap();
        file.lock().unwrap();
        // A concurrent fork inherits this same open file description until
        // exec closes it. Keep a duplicate to make that interval deterministic.
        let inherited = file.try_clone().unwrap();
        let permit = Permit { _guard: Some(file) };
        let next = OpenOptions::new().append(true).open(&lock).unwrap();
        assert!(matches!(next.try_lock(), Err(TryLockError::WouldBlock)));
        drop(permit);
        next.try_lock()
            .expect("the permit must release admission before the child execs");
        next.unlock().unwrap();
        drop(inherited);
    }

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
                if !sorting {
                    fs::write(fixture.path("active"), "").unwrap();
                    fs::write(fixture.cfg.jobs.join(id).join("1.slot"), "1").unwrap();
                }
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
            if !sorting {
                // Both workers are now policy-parked with their units gone.
                fs::remove_file(fixture.path("active")).unwrap();
            }
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

#[cfg(test)]
mod turn_tests {
    use super::*;
    use crate::inbox::sorting_tests::Box;
    use std::fs;

    #[test]
    fn a_waiting_drained_jobs_next_turn_reacquires_capacity_before_launch() {
        let mut fixture = Box::new();
        fixture.cfg.inbox.as_mut().unwrap().wake = fixture.path("wake.json");
        fixture.cfg.kick.clear();
        fixture.cfg.notify = None;
        let active = fixture.path("active");
        burrow_test::executable(
            &fixture.cfg.tools.systemctl,
            format!(
                "#!/bin/sh\ncase \"$*\" in\n*is-active*) test -e '{}';;\n*LoadState*) echo not-found;;\n*ActiveState*) echo inactive;;\nesac\n",
                active.display()
            ),
        );

        let launched = fixture.path("launched");
        fixture.cfg.tools.systemd_run = fixture.path("systemd-run");
        burrow_test::executable(
            &fixture.cfg.tools.systemd_run,
            format!("#!/bin/sh\ntouch '{}'\n", launched.display()),
        );
        for id in ["waiting", "active"] {
            fixture.job_ending(id, "done", Some("await"), "kept");
            let d = fixture.cfg.inbox.as_ref().unwrap().state.join(id);
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("drained"), "sam/box#1").unwrap();
            fs::write(d.join("job"), id).unwrap();
        }
        fs::write(fixture.path("active"), "").unwrap();
        fs::write(fixture.cfg.jobs.join("active/1.slot"), "1").unwrap();
        let job = Job::new(&fixture.cfg, "waiting");
        let held = job.lock().unwrap();
        crate::turn::start(&fixture.cfg, job.id, "the await changed", false).unwrap();
        drop(held);
        assert!(crate::policy::parked(&job));
        assert_eq!(job.turns().as_deref(), Some("2"));
        assert!(
            !launched.exists(),
            "capacity-full resume must not start a unit"
        );
        fs::remove_file(fixture.path("active")).unwrap();
        crate::policy::reconcile(&fixture.cfg).unwrap();
        assert!(launched.exists());
        assert_eq!(
            job.turns().as_deref(),
            Some("2"),
            "resume the queued turn itself"
        );
    }
}
