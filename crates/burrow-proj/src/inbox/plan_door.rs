//! `proj plan`, the host's door into the task lists for an author
//! ([`super::plans`] keeps them).

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::path::PathBuf;

use super::{FORGET_WAIT, Pass, plans, projects, resumption};
use crate::config::Config;
use crate::held::Held;
use crate::{Fail, io_fail};

/// `proj plan`: one of the inbox's authors, on the host and never from a
/// job, changes a task list, and the list moves on at its repo's next pass.
/// `done OWNER/REPO#N PR-URL` records the merged PR that landed a child,
/// `done OWNER/REPO#N --no-pr REASON` why a closed child needed none;
/// `end OWNER/REPO#N` ends a parent's list (one wrongly split, or whose
/// parent is a tracker) without closing the parent. The author is the
/// GitHub account the caller's own gh answers as.
pub fn plan(cfg: &Config, args: &[String]) -> Result<(), Fail> {
    /// What `proj plan` is asked to do.
    enum Asked<'a> {
        Done(&'a str, &'a str),
        DoneWithoutPr(&'a str, String),
        End(&'a str),
    }
    let usage = || Fail::new(2, plans::PLAN_USAGE);
    let (door, asked) = match args {
        [done, child, pr] if done == "done" && pr != "--no-pr" => ("done", Asked::Done(child, pr)),
        [done, child, flag, reason @ ..] if done == "done" && flag == "--no-pr" => {
            let reason = reason.join(" ");
            let reason = reason.trim();
            if reason.is_empty() || reason.contains('\n') {
                return Err(usage());
            }
            ("done", Asked::DoneWithoutPr(child, reason.to_owned()))
        }
        [end, parent] if end == "end" => ("end", Asked::End(parent)),
        _ => return Err(usage()),
    };
    if std::env::var_os("PROJ_JOB").is_some_and(|id| !id.is_empty()) {
        return Err(Fail::new(
            2,
            format!(
                "proj plan {door} is the host's, for an author: a job never changes a task list"
            ),
        ));
    }
    let inbox =
        cfg.inbox.as_ref().filter(|i| i.planning).ok_or_else(|| {
            Fail::new(1, format!("proj plan {door}: inbox planning isn't on here"))
        })?;
    let caller = crate::github::reported(burrow_github::call_repeating(
        &mut burrow_github::Gh::default(),
        "GET",
        "user",
        None,
    ))
    .and_then(|r| r.json().ok())
    .and_then(|v| v["id"].as_u64());
    // Under the pass lock, as passes and `forget` are: the child's place on
    // its list and the record land together, with no pass writing beside it.
    let lock = PathBuf::from(format!("{}.lock", inbox.wake.display()));
    let held = OpenOptions::new()
        .append(true)
        .create(true)
        .open(&lock)
        .map_err(|e| io_fail(&lock, e))?;
    let Some(held) = Held::lock_within(held, FORGET_WAIT).map_err(|e| io_fail(&lock, e))? else {
        return Err(Fail::new(
            1,
            format!(
                "proj plan {door}: an inbox pass is still going after ten minutes; nothing changed"
            ),
        ));
    };
    let permit = resumption::Permit::pass(held);
    let mut p = Pass {
        cfg,
        inbox,
        project: projects(cfg),
        wake: BTreeMap::new(),
        unreached: Vec::new(),
        looked: Vec::new(),
        observe: false,
        scope: None,
        drain_at: None,
        skipped: Vec::new(),
        closing_prs: BTreeMap::new(),
    };
    let (repo, said) = match asked {
        Asked::Done(child, pr) => (
            p.plan_done(child, pr, caller)?,
            format!(
                "Recorded {pr} as {child}'s landed PR; its task list moves on at the repo's next pass."
            ),
        ),
        Asked::DoneWithoutPr(child, reason) => (
            p.plan_done_without_pr(child, &reason, caller)?,
            format!(
                "Recorded {child} as done without a PR; its task list moves on at the repo's next pass."
            ),
        ),
        Asked::End(parent) => (
            p.plan_end(parent, caller)?,
            format!(
                "Ended {parent}'s task list; the parent stays open, its children no longer start, and the list is kept on disk."
            ),
        ),
    };
    drop(permit);
    crate::job::kick(cfg, [format!("proj-inbox/{repo}")]);
    println!("{said}");
    Ok(())
}
