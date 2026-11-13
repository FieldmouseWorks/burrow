//! Relaunch authority lives in the sandbox-denied host control directory.
use super::*;
use std::collections::BTreeMap;

#[derive(Serialize, Deserialize)]
pub(crate) struct Identity {
    pub choice: engine::Choice,
    /// The job's request files as this record's turn began. The protected
    /// request ([`crate::job::Request`]) is the authority; this copy is kept
    /// in the earlier format so a rolled-back proj's transfer consumer can
    /// still restore a retained retry, and so migration can read captures an
    /// earlier proj wrote.
    #[serde(default)]
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
        let choice = engine::requested(
            job.cfg,
            job,
            &crate::job::Request::load(job)?,
            recorded.as_deref(),
        )?;
        Self::save(job, n, choice)
    }
    fn save(job: &Job, n: &str, choice: engine::Choice) -> Result<(), String> {
        crate::ask::ensure(job)?;
        swap(
            &job.turn(n, "execution"),
            &serde_json::to_string(&Self {
                choice,
                files: crate::job::Request::load(job)?.captured_files(),
            })
            .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())
    }
    /// What an earlier proj captured of the job's request files, across every
    /// execution record on the host, never just the one the (job-writable)
    /// turn selector names, so advancing or rewriting `turns` cannot hide a
    /// capture. Records are read under their actual filenames, padded turn
    /// numbers included. What the starter fixed (project, app, agent, owned)
    /// comes from the earliest capture; engine and role, which the host
    /// changes when the inbox enrolls a job, from the latest, so migration
    /// keeps a completed enrollment without waiting for an inbox pass.
    pub fn legacy_files(job: &Job) -> Result<Option<BTreeMap<String, Option<String>>>, String> {
        let dir = crate::ask::host_path(job, "request");
        let entries = match fs::read_dir(dir.parent().unwrap_or(&dir)) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        let mut names: Vec<(u64, String)> = entries
            .filter_map(Result::ok)
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let stem = name.strip_suffix(".execution")?;
                let n: u64 = stem.parse().ok()?;
                (n > 0 && crate::job::valid_turn(stem)).then(|| (n, stem.to_owned()))
            })
            .collect();
        names.sort_unstable();
        let mut captures = Vec::new();
        for (_, stem) in names {
            if let Some(identity) = Self::load(job, &stem)? {
                if !identity.files.is_empty() {
                    captures.push(identity.files);
                }
            }
        }
        let Some(latest) = captures.last().cloned() else {
            return Ok(None);
        };
        let mut files = captures.remove(0);
        for key in ["engine", "role"] {
            match latest.get(key) {
                Some(value) => files.insert(key.into(), value.clone()),
                None => files.remove(key),
            };
        }
        Ok(Some(files))
    }
    pub fn restore(job: &Job, n: &str) -> Result<(), String> {
        let Some(identity) = Self::load(job, n)? else {
            return Err("retained retry has no host execution identity".into());
        };
        crate::job::Request::load(job)?
            .publish(job)
            .map_err(|e| e.to_string())?;
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
/// proves the trusted unit parent waits in the fenced legacy census: blocked
/// on its lock, or polling it ([`polling`]). A running sandbox, or a parent
/// completing one, cannot manufacture either host-side scheduler state by
/// editing its job files.
pub(super) fn legacy_waiting(cfg: &Config, pid: u32) -> io::Result<bool> {
    let want = lock_id(&fs::metadata(cfg.jobs.join(".slots/.lock"))?);
    if polling(pid, &want) {
        return Ok(true);
    }
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

/// A nonblocking scheduler (3bdabc2's) never blocks on the census: each pass
/// opens it, tries the lock and, refused, sleeps two seconds with the file
/// still open. Its retries wait there with no `->` entry in /proc/locks. In
/// every proj, only the wait for a slot opens the census from Rust, and Rust
/// opens every file close-on-exec, so such a descriptor in the trusted
/// parent's own table is the kernel's word that it is in that wait. The fd 9
/// a handover's shell leaves open for the parent's whole life (`_exec` only
/// unlocks it) has no close-on-exec flag and proves nothing; nor can a
/// sandbox add a descriptor to its host parent's table.
fn polling(pid: u32, want: &str) -> bool {
    let Ok(fds) = fs::read_dir(format!("/proc/{pid}/fd")) else {
        return false;
    };
    fds.filter_map(Result::ok).any(|fd| {
        fs::metadata(fd.path()).is_ok_and(|meta| lock_id(&meta) == want)
            && fs::read_to_string(format!(
                "/proc/{pid}/fdinfo/{}",
                fd.file_name().to_string_lossy()
            ))
            .is_ok_and(|info| close_on_exec(&info))
    })
}

/// fdinfo's `flags:`, in octal, carries O_CLOEXEC for a close-on-exec fd.
pub(super) fn close_on_exec(fdinfo: &str) -> bool {
    fdinfo
        .lines()
        .find_map(|line| line.strip_prefix("flags:"))
        .and_then(|flags| u32::from_str_radix(flags.trim(), 8).ok())
        .is_some_and(|flags| flags & rustix::fs::OFlags::CLOEXEC.bits() != 0)
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
        && target.is_some_and(|path| {
            same(path, &cfg.deployed) || store_tool(path, "proj") || store_tool(path, "burrow-proj")
        })
        && args[6] == b"_exec"
}
