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

/// Whether the parent holds a pool slot, by the kernel's lock table: a turn
/// that runs holds a lock on its jobs/.slots/N file (open across everything
/// it starts), a waiting one holds none. N.slot, outcome and the rest of a job
/// directory are the sandbox's to write, so none of them says it is waiting.
pub(super) fn holds_slot(cfg: &Config, pid: u32) -> io::Result<bool> {
    let mut held = std::collections::HashSet::new();
    for pool in [".slots", ".codex-slots"] {
        let entries = match fs::read_dir(cfg.jobs.join(pool)) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        for entry in entries.filter_map(Result::ok) {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name != "unstick" && !name.bytes().all(|c| c.is_ascii_digit()) {
                continue;
            }
            if let Ok(meta) = fs::metadata(entry.path()) {
                held.insert(lock_id(&meta));
            }
        }
    }
    let locks = fs::read_to_string("/proc/locks")?;
    Ok(locks.lines().any(|line| {
        let fields: Vec<_> = line.split_whitespace().collect();
        fields.len() >= 6
            && fields[1] == "FLOCK"
            && fields[4] == pid.to_string()
            && held.contains(fields[5])
    }))
}

fn lock_id(meta: &fs::Metadata) -> String {
    use std::os::unix::fs::MetadataExt;
    format!(
        "{:02x}:{:02x}:{}",
        rustix::fs::major(meta.dev()),
        rustix::fs::minor(meta.dev()),
        meta.ino()
    )
}

/// Earlier parents have no host identity. Adopt them only while the kernel
/// proves the trusted unit parent is blocked in the fenced legacy census.
/// A running sandbox, or a parent completing one, cannot manufacture that
/// host-side scheduler state by editing its job files.
pub(super) fn legacy_waiting(cfg: &Config, pid: u32) -> io::Result<bool> {
    let want = lock_id(&fs::metadata(cfg.jobs.join(".slots/.lock"))?);
    // The handover shell waits in its flock child.
    let mut waiters = vec![pid.to_string()];
    let handover = fs::read_link(format!("/proc/{pid}/exe"))
        .ok()
        .zip(fs::read(format!("/proc/{pid}/cmdline")).ok())
        .is_some_and(|(exe, bytes)| {
            legacy_handover(cfg, &exe, &bytes.split(|b| *b == 0).collect::<Vec<_>>())
        });
    if handover && let Ok(children) = fs::read_to_string(format!("/proc/{pid}/task/{pid}/children"))
    {
        waiters.extend(children.split_whitespace().map(str::to_owned));
    }
    let locks = fs::read_to_string("/proc/locks")?;
    Ok(locks.lines().any(|line| {
        let fields: Vec<_> = line.split_whitespace().collect();
        fields.len() >= 9
            && fields[1] == "->"
            && fields[2] == "FLOCK"
            && waiters.iter().any(|w| w == fields[5])
            && fields[6] == want
    }))
}

/// An old parent's `hand_over` shell: `bash -c SCRIPT LOCK FLOCK PROJ _exec
/// ID CLASS`, exactly. It is the unit's main process until the shell has
/// taken the lock and executed the replacement, and the barrier can hold that
/// lock first. Anything else named bash, or any other argv, is not one.
pub(super) fn legacy_handover(cfg: &Config, exe: &Path, args: &[&[u8]]) -> bool {
    const SCRIPT: &[u8] = br#"exec 9>"$0"; "$1" 9; shift; exec "$@""#;
    use std::os::unix::ffi::OsStrExt;
    let same = |path: &Path, want: &Path| {
        fs::canonicalize(path)
            .ok()
            .zip(fs::canonicalize(want).ok())
            .is_some_and(|(a, b)| a == b)
    };
    let store_tool = |path: &Path, name: &str| {
        path.starts_with("/nix/store")
            && !path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
            && path.file_name().is_some_and(|n| n == name)
    };
    let flock = args
        .get(4)
        .and_then(|b| std::str::from_utf8(b).ok())
        .map(Path::new);
    let target = args
        .get(5)
        .and_then(|b| std::str::from_utf8(b).ok())
        .map(Path::new);
    args.len() >= 9
        && args[1] == b"-c"
        && args[2] == SCRIPT
        && (same(exe, &cfg.tools.bash) || store_tool(exe, "bash"))
        && args[3] == cfg.jobs.join(".slots/.lock").as_os_str().as_bytes()
        && (args[4] == cfg.tools.flock.as_os_str().as_bytes()
            || flock.is_some_and(|path| store_tool(path, "flock")))
        && target.is_some_and(|path| store_tool(path, "burrow-proj") || store_tool(path, "proj"))
        && args[6] == b"_exec"
}
