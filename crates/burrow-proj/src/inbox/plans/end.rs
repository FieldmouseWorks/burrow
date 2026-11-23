//! Ending a task list without closing its parent: a list split from what
//! was never one piece of work (a bot's page, [`super::super::maintained`]),
//! or one an author ends with `proj plan end`. The list is replanned to
//! nothing: its tasks and children are retained on disk, so none of its
//! children starts again ([`super::Place::Superseded`]), its checklist says
//! so, and its pause and the owner's notice go.
//!
//! An end is begun durably ([`ENDING`], the text it ends on) before any of
//! that, and each pass finishes one begun ([`Pass::finish_end`]), so one
//! GitHub cut short (its checklist, say) is carried through, not left half
//! done with nothing to retry it.

use super::Pass;
use crate::inbox::key;
use crate::inbox::record::files;
use crate::{Fail, swap};
use std::fs;
use std::io::ErrorKind::NotFound;

/// In a parent's plan folder while its list's end is begun and not yet
/// carried through: the parent's text it ends on, exactly.
const ENDING: &str = "ending";

impl Pass<'_> {
    /// What could still write parent N's list back after it ended: its own
    /// job, live or with its record kept (which carries out the job's `proj
    /// split`), or a sorting job's verdict on it being carried out. None
    /// when nothing could.
    pub(in crate::inbox) fn list_writer(&self, repo: &str, n: &str) -> Option<String> {
        if let Some(id) = self.owner(repo, n) {
            return Some(format!("its own job {id} is still live"));
        }
        if self.started_issue_job(Some(repo), n) {
            return Some("the inbox still keeps its own job, whose split may be pending".into());
        }
        let sorting = files::sorting(&self.inbox.state, &key(repo), n);
        if files::has_job(&sorting) {
            return Some("a sorting job's verdict on it is still being carried out".into());
        }
        None
    }

    /// Whether parent N's list's end is begun and not yet carried through.
    pub(in crate::inbox) fn ending(&self, repo: &str, n: &str) -> bool {
        self.plan_dir(repo, n).join(ENDING).exists()
    }

    /// Parent N's list ended on `input` (the parent's text as it reads
    /// now), begun durably first. The parent stays open, sorted as it is.
    /// Whether every step held; one that didn't is finished by the next
    /// pass ([`Pass::finish_end`]), or by asking again.
    pub(in crate::inbox) fn end_list(&mut self, repo: &str, n: &str, input: &str) -> bool {
        let d = self.plan_dir(repo, n);
        if let Err(e) = swap(&d.join(ENDING), input) {
            eprintln!(
                "proj-inbox: couldn't keep {}: {e}",
                d.join(ENDING).display()
            );
            return false;
        }
        self.finish_end(repo, n)
    }

    /// Parent N's begun end ([`ENDING`]) carried through, every step safe
    /// to repeat: the list replanned to nothing on the text it ends on, the
    /// owner's notice closed (its close retried by
    /// [`Pass::settle_notices`] until the notifier confirms it), the pause
    /// and what held the list gone, and the end then no longer pending.
    /// Whether all of it held; true when no end is pending.
    pub(in crate::inbox) fn finish_end(&mut self, repo: &str, n: &str) -> bool {
        let d = self.plan_dir(repo, n);
        let input = match fs::read_to_string(d.join(ENDING)) {
            Ok(input) => input,
            Err(e) if e.kind() == NotFound => return true,
            Err(e) => {
                eprintln!(
                    "proj-inbox: couldn't read {}: {e}",
                    d.join(ENDING).display()
                );
                return false;
            }
        };
        if !self.prepare_plan(repo, n, &input, &[]) {
            return false;
        }
        self.reconcile_notice(repo, n, None);
        self.pause(repo, n, &d, false);
        for f in ["held", ENDING] {
            match fs::remove_file(d.join(f)) {
                Ok(()) => {}
                Err(e) if e.kind() == NotFound => {}
                Err(e) => {
                    eprintln!("proj-inbox: couldn't remove {}: {e}", d.join(f).display());
                    return false;
                }
            }
        }
        true
    }

    /// `proj plan end`'s work, under the inbox's pass lock: `caller` is the
    /// GitHub account it runs as. Answers the repo as its clone spells it.
    pub(crate) fn plan_end(&mut self, parent: &str, caller: Option<u64>) -> Result<String, Fail> {
        let (repo, n) = self.plan_door("end", parent, caller)?;
        let n = n.to_string();
        if !self.has_task_list(Some(&repo), &n) && !self.ending(&repo, &n) {
            return Err(Fail::new(
                1,
                format!("proj plan end: {repo}#{n} has no task list to end"),
            ));
        }
        if let Some(why) = self.list_writer(&repo, &n) {
            return Err(Fail::new(
                1,
                format!(
                    "proj plan end: {repo}#{n}'s list isn't ended while {why}; nothing changed"
                ),
            ));
        }
        let issue = crate::github::api(self.cfg, &format!("repos/{repo}/issues/{n}"))
            .ok_or_else(|| Fail::new(1, format!("proj plan end: couldn't read {repo}#{n}")))?;
        if !self.eligible(&repo, &issue) {
            return Err(Fail::new(
                1,
                format!("proj plan end: {repo}#{n} isn't an issue the inbox may read"),
            ));
        }
        let input = self.input(&repo, &issue).ok_or_else(|| {
            Fail::new(1, format!("proj plan end: couldn't read {repo}#{n}'s text"))
        })?;
        if !self.end_list(&repo, &n, &input) {
            return Err(Fail::new(
                1,
                format!(
                    "proj plan end: {repo}#{n}'s list is ending but GitHub didn't take every step; its repo's next pass, or asking again, finishes it"
                ),
            ));
        }
        Ok(repo)
    }
}
