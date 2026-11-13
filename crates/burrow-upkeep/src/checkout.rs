//! A job sees a clone only after gh and all its descendants have finished.
//! This command owns its deadline: its caller must let it reap and clean up
//! rather than killing the supervisor at the same deadline as the clone.
use rustix::process::{WaitOptions, getpid, set_child_subreaper, wait};
use rustix::time::{ClockId, clock_gettime};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

pub fn clone(repo: &str, path: &Path, until: f64) -> Result<(), String> {
    let now = clock_gettime(ClockId::Monotonic);
    let remaining = until - (now.tv_sec as f64 + now.tv_nsec as f64 / 1e9);
    if remaining <= 0.0 {
        return Err("clone deadline reached".into());
    }
    let until = Instant::now() + Duration::from_secs_f64(remaining);
    // Only this short-lived command adopts children, never the steward or
    // unrelated upkeep operations. gh, git and transports share the process
    // group that burrow-process kills on every exit, including gh failing.
    set_child_subreaper(Some(getpid())).map_err(|e| e.to_string())?;
    let parent = path.parent().ok_or("checkout has no parent")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let staging = tempfile::Builder::new()
        .prefix(".autoland-clone-")
        .tempdir_in(parent)
        .map_err(|e| e.to_string())?;
    // No .git directly under the staging root: projects() cannot adopt it,
    // even for a repository whose name begins with a dot.
    let tree = staging.path().join("tree");
    let result = burrow_process::output(
        Command::new("gh").args(["repo", "clone", repo]).arg(&tree),
        None,
        until,
        1024 * 1024,
    );
    // The group has been killed and its leader waited for. Reap adopted
    // descendants before inspecting or removing anything they could write.
    loop {
        match wait(WaitOptions::empty()) {
            Ok(Some(_)) => continue,
            Err(rustix::io::Errno::INTR) => continue,
            Err(rustix::io::Errno::CHILD) => break,
            other => return Err(format!("reaping clone descendants: {other:?}")),
        }
    }
    let out = result?;
    if !out.status.success() {
        // Neither command output nor arguments belong in the journal: they
        // can contain credentials. Failure never publishes a partial clone.
        return Err(format!("gh repo clone exited {}", out.status));
    }
    if Instant::now() >= until {
        return Err("clone deadline reached".into());
    }
    // Directory rename publishes atomically, can replace an empty folder,
    // and refuses nonempty folders, files and symlinks. Never remove a
    // pre-existing destination, even if another writer filled it meanwhile.
    fs::rename(&tree, path).map_err(|e| format!("publishing checkout: {e}"))?;
    Ok(())
}
