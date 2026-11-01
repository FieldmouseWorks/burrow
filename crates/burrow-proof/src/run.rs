//! `burrow-proof run DIR`, the run's unit's own process: each check's
//! command in turn, by name, in bash, in a fresh worktree of the repo's
//! checkout at the PR's head, inside its fence. Its environment is the
//! unit's, what the machine user's own units start with (its manager's
//! PATH, as proj's jobs get it), and PROOF_REPO, PROOF_NUMBER, PROOF_HEAD,
//! PROOF_CHECK and PROOF_SUMMARY (the file whose words go on the PR). What
//! it prints goes to the run's log, on this machine only. It takes no
//! heavy slot: a command takes the locks it needs itself.
//!
//! Done once each check's command has run and given a verdict ([`verdict`]):
//! `comment.md`, then `ran.json`, which `end` reads. A head that can't be
//! fetched, a worktree or fence that can't be made or cleared, or a
//! command whose exit is the machine's failure ends it short, with neither:
//! `end` then says it failed, and the steward runs it again later.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::fence::Fence;
use crate::{
    CGROUP, COMMENT, LOG, RAN, Ran, Record, comment, save, summary, trees, verdict, write_whole,
};

/// The run whose folder is DIR, each command fenced by FENCE. An error is
/// why it ended short, in words, also in its log.
pub fn run(dir: &Path, fence: &mut dyn Fence) -> Result<(), String> {
    let rec = Record::read(dir)?;
    let id = id(dir)?;
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(LOG))
        .map_err(|e| format!("{}: {e}", dir.join(LOG).display()))?;
    let note = |text: &str| {
        // The log is the record; a line it won't take is no reason to stop.
        let _ = writeln!(&log, "autoland: {text}");
    };
    let short = |why: String| {
        note(&why);
        Err(why)
    };
    // Where its fences are, before any worktree: other runs' sweeps leave
    // its worktrees alone for as long as that cgroup is there.
    if let Some(at) = fence.record() {
        let at = at.to_string_lossy().into_owned();
        if let Err(e) = write_whole(&dir.join(CGROUP), at.as_bytes()) {
            return short(format!("can't record its cgroup: {e}"));
        }
    }
    let builds = dir.parent().unwrap_or(dir);
    for said in trees::sweep(&rec.trees, builds, id, &rec.git) {
        note(&said);
    }
    if !fetched(&rec, &log) {
        return short(format!(
            "can't fetch {} into {}",
            rec.head,
            rec.checkout.display()
        ));
    }
    let mut ran = Ran::default();
    for (i, (check, cmd)) in rec.runs.iter().enumerate() {
        note(&format!("{check}: {cmd}"));
        let exit = match one(&rec, dir, id, i, check, cmd, fence, &log) {
            Ok(exit) => exit,
            Err(why) => return short(format!("{check}: {why}")),
        };
        note(&format!("{check}: exit {exit}"));
        let Some(passed) = verdict(exit) else {
            return short(format!(
                "{check}: exit {exit} is the machine failing, not a verdict"
            ));
        };
        let said = if passed {
            ["success".to_owned(), "Ran here and passed".to_owned()]
        } else {
            [
                "failure".to_owned(),
                format!("Ran here and failed: exit {exit}"),
            ]
        };
        ran.verdicts.insert(check.clone(), said);
        ran.exits.insert(check.clone(), exit);
    }
    if let Some(text) = comment::comment(&rec, dir, id, &ran)
        && let Err(e) = write_whole(&dir.join(COMMENT), text.as_bytes())
    {
        return short(format!("can't write its comment: {e}"));
    }
    if let Err(e) = save(&dir.join(RAN), &ran) {
        return short(format!("can't write how it ran: {e}"));
    }
    Ok(())
}

/// The run's id: its folder's name.
pub fn id(dir: &Path) -> Result<&str, String> {
    dir.file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("{} names no run", dir.display()))
}

/// The PR's head fetched into the checkout (GitHub keeps it as
/// refs/pull/N/head), and there: nothing else of origin's.
fn fetched(rec: &Record, log: &File) -> bool {
    let git = |args: &[&str], out: Stdio, err: Stdio| -> bool {
        Command::new(&rec.git)
            .arg("-C")
            .arg(&rec.checkout)
            .args(args)
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err)
            .status()
            .is_ok_and(|s| s.success())
    };
    let (Ok(out), Ok(err)) = (log.try_clone(), log.try_clone()) else {
        return false;
    };
    let pull = format!("refs/pull/{}/head", rec.number);
    git(&["fetch", "-q", "origin", &pull], out.into(), err.into())
        && git(
            &["cat-file", "-e", &format!("{}^{{commit}}", rec.head)],
            Stdio::null(),
            Stdio::null(),
        )
}

/// The I-th check's command, CMD, in its worktree and fence: its exit, a
/// signal's as a shell has it (128 and up). By the time this returns, its
/// fence is empty and its worktree gone.
#[allow(clippy::too_many_arguments)]
fn one(
    rec: &Record,
    dir: &Path,
    id: &str,
    i: usize,
    check: &str,
    cmd: &str,
    fence: &mut dyn Fence,
    log: &File,
) -> Result<i32, String> {
    let tree = rec.trees.join(format!("{id}-{i}"));
    let made = trees::add(&rec.git, &rec.checkout, &tree, &rec.head, log);
    if !made.as_ref().is_ok_and(|ok| *ok) {
        let _ = trees::remove(&rec.git, &rec.checkout, &tree);
        return Err(match made {
            Err(e) => format!("can't make its worktree at {}: {e}", tree.display()),
            Ok(_) => format!("can't make its worktree at {}", tree.display()),
        });
    }
    let ended = fenced(rec, dir, i, check, cmd, fence, log, &tree);
    // Its fence first, then its worktree: nothing of it can still be in
    // there. A fence that won't clear keeps its worktree, for `end` to
    // remove once systemd has ended every process of the unit.
    fence
        .clear(i)
        .map_err(|e| format!("can't end what it left: {e}"))?;
    if let Err(e) = trees::remove(&rec.git, &rec.checkout, &tree) {
        let _ = writeln!(
            &mut &*log,
            "autoland: {check}: can't remove its worktree: {e}"
        );
    }
    ended
}

#[allow(clippy::too_many_arguments)]
fn fenced(
    rec: &Record,
    dir: &Path,
    i: usize,
    check: &str,
    cmd: &str,
    fence: &mut dyn Fence,
    log: &File,
    tree: &Path,
) -> Result<i32, String> {
    let summary = summary(dir, i);
    let _ = fs::remove_file(&summary);
    let mut c = fence
        .around(i, &rec.bash, &["-c", cmd])
        .map_err(|e| format!("can't make its fence: {e}"))?;
    let (out, err) = (log.try_clone(), log.try_clone());
    let (Ok(out), Ok(err)) = (out, err) else {
        return Err("can't hand it its log".into());
    };
    c.current_dir(tree)
        .env("PROOF_REPO", &rec.repo)
        .env("PROOF_NUMBER", rec.number.to_string())
        .env("PROOF_HEAD", &rec.head)
        .env("PROOF_CHECK", check)
        .env("PROOF_SUMMARY", &summary)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    let status = c
        .status()
        .map_err(|e| format!("can't start {}: {e}", rec.bash.display()))?;
    Ok(status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or_default()))
}
