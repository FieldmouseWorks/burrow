//! What the starter authorized a job to run as: its project, pinned engine,
//! role, GitHub App, sandbox and ownership. The record lives in the job's
//! host control directory, which no sandbox reaches, and is the only
//! authority for every turn and every engine run within one. The job
//! directory's files of the same names are its published copy, for readers
//! such as the probe and `proj status`: a job may write those, and nothing
//! proj runs it as follows them.
use super::Job;
use crate::{engine::Engine, read};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, io, path::PathBuf};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Request {
    pub project: String,
    pub engine: Option<Engine>,
    pub role: Option<String>,
    pub app: Option<String>,
    pub agent: Option<String>,
    pub owned: bool,
}

impl Request {
    fn path(job: &Job) -> PathBuf {
        crate::ask::host_path(job, "request")
    }

    /// The job's request. A job started before the record existed adopts
    /// what was last captured for it on the host (a retained retry's
    /// identity), else its published files, once: from then on the record
    /// holds, as `policy`'s admission does.
    pub fn load(job: &Job) -> Result<Self, String> {
        match crate::state_reads::bytes(&Self::path(job)) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| e.to_string()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let captured = job
                    .turns()
                    .map(|n| crate::retry::Identity::load(job, &n))
                    .transpose()?
                    .flatten()
                    .map(|identity| identity.legacy_files().clone())
                    .filter(|files| !files.is_empty());
                let request = match captured {
                    Some(files) => Self::from(|key| files.get(key).cloned().flatten())?,
                    None => Self::from(|key| read(&job.path(key)))?,
                };
                request.save(job)?;
                Ok(request)
            }
            Err(e) => Err(e.to_string()),
        }
    }

    fn from(read: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let named = |key| read(key).filter(|s: &String| !s.is_empty());
        Ok(Self {
            project: read("project").unwrap_or_default(),
            engine: named("engine").map(|e| Engine::parse(&e)).transpose()?,
            role: named("role"),
            app: named("app"),
            agent: named("agent"),
            owned: read("owned").is_some(),
        })
    }

    /// Only the starter and host-side changes it authorizes (an inbox
    /// label's role) call this; it publishes the copy too.
    pub fn save(&self, job: &Job) -> Result<(), String> {
        crate::ask::ensure(job)?;
        crate::swap(
            &Self::path(job),
            &serde_json::to_string(self).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        self.publish(job).map_err(|e| e.to_string())
    }

    /// The machine's label maps the job to `role`: the role's own engine,
    /// not the pin its old label gave it.
    pub fn enroll(job: &Job, role: String) -> Result<(), String> {
        let mut request = Self::load(job)?;
        if request.role.as_deref() == Some(role.as_str()) && request.engine.is_none() {
            return Ok(());
        }
        request.role = Some(role);
        request.engine = None;
        request.save(job)
    }

    /// The published copy, put back as the record has it.
    pub fn publish(&self, job: &Job) -> io::Result<()> {
        let files: BTreeMap<&str, Option<String>> = BTreeMap::from([
            (
                "project",
                (!self.project.is_empty()).then(|| format!("{}\n", self.project)),
            ),
            ("engine", self.engine.map(|e| e.name().to_owned())),
            ("role", self.role.as_ref().map(|r| format!("{r}\n"))),
            ("app", self.app.as_ref().map(|a| format!("{a}\n"))),
            ("agent", self.agent.as_ref().map(|a| format!("{a}\n"))),
            ("owned", self.owned.then(String::new)),
        ]);
        for (key, value) in files {
            let path = job.path(key);
            if read(&path).as_deref() == value.as_deref().map(|v| v.trim_end_matches('\n')) {
                continue;
            }
            match value {
                Some(value) => crate::state_reads::write_public(&path, value)?,
                None => match crate::state_reads::remove(&path) {
                    Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                    _ => {}
                },
            }
        }
        Ok(())
    }
}
