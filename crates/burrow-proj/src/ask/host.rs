//! Host-owned question authority, outside every job's writable state.
use crate::{config::Config, job::Job};
use std::{fs, io::Write, os::unix::fs::DirBuilderExt, path::PathBuf};

pub(crate) fn control_root(cfg: &Config) -> PathBuf {
    cfg.host_state
        .clone()
        .unwrap_or_else(|| cfg.jobs.join(".question-control"))
}
pub(crate) fn host_path(job: &Job, name: &str) -> PathBuf {
    control_root(job.cfg).join(job.id).join(name)
}
pub(crate) fn ensure(job: &Job) -> Result<(), String> {
    for path in [control_root(job.cfg), host_path(job, "")] {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&path)
            .map_err(|e| e.to_string())?;
        if !fs::symlink_metadata(&path)
            .map_err(|e| e.to_string())?
            .is_dir()
        {
            return Err(format!("{} is not a host state directory", path.display()));
        }
    }
    Ok(())
}

/// Written by the authenticated starter, never by a guest declaration.
pub(crate) fn initialize(job: &Job, link: Option<&str>) -> Result<(), String> {
    ensure(job)?;
    if let Some((repo, number)) = link.and_then(crate::github::target) {
        let path = host_path(job, "source");
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| e.to_string())?;
        writeln!(file, "{repo}#{number}").map_err(|e| e.to_string())?;
    }
    Ok(())
}
