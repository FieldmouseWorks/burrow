//! How the starter said a job is scheduled and bounded: `--unstick` (the
//! emergency slot, past the drain's limits), `--pipeline` (its class), its
//! turns' `--timeout` and `--idle`, what each turn's start and end kick, and
//! the `--link` its GitHub quota is held to. Kept beside its
//! [`super::Request`] in the job's host control directory, which no sandbox
//! reaches: only the starter and the host-side changes it authorizes
//! (`proj unstick`) write it. The job directory's files of the same names
//! are its published copy, put back at each turn's start; a job may write
//! those, and nothing proj schedules or bounds it by follows them.
use super::{Job, record};
use crate::read;
use serde::{Deserialize, Serialize};
use std::{io, path::PathBuf};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Schedule {
    pub unstick: bool,
    pub pipeline: bool,
    pub timeout: Option<String>,
    pub idle: Option<String>,
    pub kick: Vec<String>,
    pub link: Option<String>,
}

impl Schedule {
    fn path(job: &Job) -> PathBuf {
        crate::ask::host_path(job, "schedule")
    }

    /// The job's schedule. A job started before the record existed adopts
    /// its published files, once: from then on the record holds, as its
    /// request's does.
    pub fn load(job: &Job) -> Result<Self, String> {
        if let Some(schedule) = record::load(&Self::path(job))? {
            return Ok(schedule);
        }
        let file = |key| read(&job.path(key));
        let adopted = Self {
            unstick: job.path("unstick").exists(),
            pipeline: job.path("pipeline").exists(),
            timeout: file("timeout"),
            idle: file("idle"),
            kick: crate::turn::lines(&job.path("kick")),
            link: file("link"),
        };
        let (winner, mine) = record::adopt(job, &Self::path(job), adopted)?;
        if mine {
            winner.publish(job).map_err(|e| e.to_string())?;
        }
        Ok(winner)
    }

    /// For a scheduling look, which can't fail a turn: a record that can't
    /// be read is the plainest schedule, with no claim to any slot or class
    /// beyond the ordinary.
    pub fn of(job: &Job) -> Self {
        Self::load(job).unwrap_or_default()
    }

    /// Only the starter and host-side changes it authorizes call this; it
    /// publishes the copy too.
    pub fn save(&self, job: &Job) -> Result<(), String> {
        record::save(job, &Self::path(job), self)?;
        self.publish(job).map_err(|e| e.to_string())
    }

    /// The record changed as a starter would have set it, for tests.
    #[cfg(test)]
    pub fn amend(job: &Job, change: impl FnOnce(&mut Self)) {
        let mut schedule = Self::load(job).unwrap();
        change(&mut schedule);
        schedule.save(job).unwrap();
    }

    /// `proj unstick`: every turn from now on is an --unstick one.
    pub fn unstick(job: &Job) -> Result<(), String> {
        let mut schedule = Self::load(job)?;
        if schedule.unstick {
            return schedule.publish(job).map_err(|e| e.to_string());
        }
        schedule.unstick = true;
        schedule.save(job)
    }

    /// The schedule a turn runs on, its published copy put back first so
    /// nothing reads what the job wrote of itself since.
    pub fn restore(job: &Job) -> Result<Self, String> {
        let schedule =
            Self::load(job).map_err(|e| format!("couldn't read the job's schedule: {e}"))?;
        schedule
            .publish(job)
            .map_err(|e| format!("couldn't publish the job's schedule: {e}"))?;
        Ok(schedule)
    }

    /// The published copy, put back as the record has it.
    pub fn publish(&self, job: &Job) -> io::Result<()> {
        let line = |v: &Option<String>| v.as_ref().map(|v| format!("{v}\n"));
        record::publish(
            job,
            [
                ("unstick", self.unstick.then(String::new)),
                ("pipeline", self.pipeline.then(String::new)),
                ("timeout", line(&self.timeout)),
                ("idle", line(&self.idle)),
                (
                    "kick",
                    (!self.kick.is_empty())
                        .then(|| self.kick.iter().map(|k| format!("{k}\n")).collect()),
                ),
                ("link", line(&self.link)),
            ],
        )
    }
}

#[cfg(test)]
#[path = "schedule_tests.rs"]
mod tests;
