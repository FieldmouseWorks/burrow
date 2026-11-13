//! A job sees a clone only after gh and all its descendants have finished.
//! This command owns its deadline: its caller must let it reap and clean up
//! rather than killing the supervisor at the same deadline as the clone.
use rustix::process::{Pid, Signal, WaitOptions, getpid, kill_process, set_child_subreaper, wait};
use rustix::time::{ClockId, clock_gettime};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

/// Why a clone was not published: `Later` is a deadline that left the work
/// retryable, which the caller must not count as the repository failing.
pub enum Stop {
    Later(String),
    Failed(String),
}

fn failed(e: impl ToString) -> Stop {
    Stop::Failed(e.to_string())
}

/// How long killing and reaping leftover descendants may take. SIGKILL can't
/// be caught, so this only bounds a process stuck in the kernel.
const CLEANUP: Duration = Duration::from_secs(5);

/// Our live children, whatever process group or session they are in: the
/// subreaper adopts every orphaned descendant of the clone.
fn children() -> Result<Vec<Pid>, String> {
    let mut pids = Vec::new();
    for task in fs::read_dir("/proc/self/task").map_err(|e| e.to_string())? {
        let list = task.map_err(|e| e.to_string())?.path().join("children");
        let text = fs::read_to_string(&list).map_err(|e| format!("{}: {e}", list.display()))?;
        pids.extend(
            text.split_whitespace()
                .filter_map(|n| n.parse::<i32>().ok())
                .filter_map(Pid::from_raw),
        );
    }
    Ok(pids)
}

/// Kill and reap every descendant the clone left, including ones that started
/// a session of their own, within CLEANUP. A killed child's own children are
/// adopted in turn, so go on until none is left.
fn reap() -> Result<(), String> {
    let limit = Instant::now() + CLEANUP;
    loop {
        for pid in children()? {
            let _ = kill_process(pid, Signal::KILL);
        }
        loop {
            match wait(WaitOptions::NOHANG) {
                Ok(Some(_)) => continue,
                Ok(None) => break,
                Err(rustix::io::Errno::INTR) => continue,
                Err(rustix::io::Errno::CHILD) => return Ok(()),
                Err(e) => return Err(format!("reaping clone descendants: {e}")),
            }
        }
        if Instant::now() >= limit {
            return Err("clone descendants outlived their cleanup".into());
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

pub fn clone(repo: &str, path: &Path, until: f64) -> Result<(), Stop> {
    clone_with("gh", repo, path, until)
}

/// `clone` with the program that stands in for `gh`, so a test can give it
/// descendants of its own.
pub fn clone_with(program: &str, repo: &str, path: &Path, until: f64) -> Result<(), Stop> {
    let now = clock_gettime(ClockId::Monotonic);
    let remaining = until - (now.tv_sec as f64 + now.tv_nsec as f64 / 1e9);
    if remaining <= 0.0 {
        return Err(Stop::Later("clone deadline reached".into()));
    }
    let until = Instant::now() + Duration::from_secs_f64(remaining);
    // Only this short-lived command adopts children, never the steward or
    // unrelated upkeep operations. gh, git and transports share the process
    // group that burrow-process kills on every exit, including gh failing.
    set_child_subreaper(Some(getpid())).map_err(failed)?;
    let parent = path
        .parent()
        .ok_or_else(|| failed("checkout has no parent"))?;
    fs::create_dir_all(parent).map_err(failed)?;
    let staging = tempfile::Builder::new()
        .prefix(".autoland-clone-")
        .tempdir_in(parent)
        .map_err(failed)?;
    // No .git directly under the staging root: projects() cannot adopt it,
    // even for a repository whose name begins with a dot.
    let tree = staging.path().join("tree");
    let result = burrow_process::output(
        Command::new(program).args(["repo", "clone", repo]).arg(&tree),
        None,
        until,
        1024 * 1024,
    );
    // gh's group has been killed and its leader waited for. Kill and reap
    // the rest, in other groups or sessions too, whatever the outcome, before
    // inspecting or removing anything they could write.
    if let Err(e) = reap() {
        // A writer may still be alive: leave the staging tree, never delete
        // under it. It is hidden and can't be adopted as a checkout.
        std::mem::forget(staging);
        return Err(Stop::Failed(e));
    }
    let out = match result {
        Ok(out) => out,
        Err(e) if Instant::now() >= until => return Err(Stop::Later(e)),
        Err(e) => return Err(Stop::Failed(e)),
    };
    if !out.status.success() {
        // Neither command output nor arguments belong in the journal: they
        // can contain credentials. Failure never publishes a partial clone.
        return Err(Stop::Failed(format!("gh repo clone exited {}", out.status)));
    }
    if Instant::now() >= until {
        return Err(Stop::Later("clone deadline reached".into()));
    }
    // Directory rename publishes atomically, can replace an empty folder,
    // and refuses nonempty folders, files and symlinks. Never remove a
    // pre-existing destination, even if another writer filled it meanwhile.
    fs::rename(&tree, path).map_err(|e| failed(format!("publishing checkout: {e}")))?;
    Ok(())
}
