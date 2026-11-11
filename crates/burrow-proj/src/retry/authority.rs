//! Relaunch authority lives in the sandbox-denied host control directory.
use super::*;
use std::collections::BTreeMap;

#[derive(Serialize, Deserialize)]
pub(crate) struct Identity {
    pub choice: engine::Choice,
    files: BTreeMap<String, Option<String>>,
}
impl Identity {
    pub fn load(job: &Job, n: &str) -> Result<Option<Self>, String> {
        match crate::state_reads::bytes(&job.turn(n, "execution")) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| e.to_string()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }
    pub fn capture(job: &Job, n: &str) -> Result<(), String> {
        if Self::load(job, n)?.is_some() {
            return Ok(());
        }
        Self::capture_choice(job, n, &engine::choice(job.cfg, job)?)
    }
    pub fn capture_choice(job: &Job, n: &str, choice: &engine::Choice) -> Result<(), String> {
        if let Some(saved) = Self::load(job, n)? {
            if saved.choice != *choice {
                return Err("execution identity disagrees with resolved startup choice".into());
            }
            return Ok(());
        }
        Self::save(job, n, choice.clone())
    }
    pub fn capture_legacy(job: &Job, n: &str) -> Result<(), String> {
        if Self::load(job, n)?.is_some() {
            return Ok(());
        }
        // Called only after kernel census-wait attestation. The old host
        // parent wrote N.engine before entering its initial wait or retry.
        let recorded = read(&job.turn(n, "engine"));
        let role = read(&job.path("role")).unwrap_or_default();
        let repo = crate::pick::github_repo(job.cfg, &crate::pick::project(&job.dir));
        let choice = engine::resolve(
            job.cfg,
            &role,
            (!repo.is_empty()).then_some(repo.as_str()),
            recorded.as_deref(),
        )?;
        Self::save(job, n, choice)
    }
    fn save(job: &Job, n: &str, choice: engine::Choice) -> Result<(), String> {
        crate::ask::ensure(job)?;
        let files = ["project", "engine", "role", "app", "agent", "owned"]
            .into_iter()
            .map(|key| (key.into(), read(&job.path(key))))
            .collect();
        swap(
            &job.turn(n, "execution"),
            &serde_json::to_string(&Self { choice, files }).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())
    }
    pub fn restore(job: &Job, n: &str) -> Result<(), String> {
        let Some(identity) = Self::load(job, n)? else {
            return Err("retained retry has no host execution identity".into());
        };
        for (key, value) in identity.files {
            let path = job.path(&key);
            match value {
                Some(value) => {
                    crate::state_reads::write(&path, &value).map_err(|e| e.to_string())?
                }
                None => match crate::state_reads::remove(path) {
                    Ok(()) => (),
                    Err(e) if e.kind() == io::ErrorKind::NotFound => (),
                    Err(e) => return Err(e.to_string()),
                },
            }
        }
        crate::state_reads::write(job.turn(n, "engine"), identity.choice.engine.name())
            .map_err(|e| e.to_string())
    }
}

/// Earlier parents have no host identity. Adopt them only while the kernel
/// proves the trusted unit parent is blocked in the fenced legacy census.
/// A running sandbox, or a parent completing one, cannot manufacture that
/// host-side scheduler state by editing its job files.
pub(super) fn legacy_waiting(cfg: &Config, pid: u32) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let meta = fs::metadata(cfg.jobs.join(".slots/.lock"))?;
    let want = format!(
        "{:02x}:{:02x}:{}",
        rustix::fs::major(meta.dev()),
        rustix::fs::minor(meta.dev()),
        meta.ino()
    );
    let locks = fs::read_to_string("/proc/locks")?;
    Ok(locks.lines().any(|line| {
        let fields: Vec<_> = line.split_whitespace().collect();
        fields.len() >= 9
            && fields[1] == "->"
            && fields[2] == "FLOCK"
            && fields[5] == pid.to_string()
            && fields[6] == want
    }))
}
