//! Ending a task list without closing its parent: a list split from what
//! was never one piece of work (a bot's page, [`super::super::maintained`]),
//! or one an author ends with `proj plan end`. The list is replanned to
//! nothing: its tasks and children are retained on disk, so none of its
//! children starts again ([`super::Place::Superseded`]), its checklist says
//! so, and its pause and the owner's notice go.

use super::Pass;
use crate::Fail;
use std::fs;

impl Pass<'_> {
    /// Parent N's list ended, `input` (the parent's text as it reads now)
    /// kept as what it was ended on. The parent stays open, sorted as it
    /// is. Whether the list ended; its notice's close is retried by the
    /// pass ([`Pass::settle_notices`]) until the notifier confirms it.
    pub(in crate::inbox) fn end_list(&mut self, repo: &str, n: &str, input: &str) -> bool {
        if !self.prepare_plan(repo, n, input, &[]) {
            return false;
        }
        self.reconcile_notice(repo, n, None);
        let d = self.plan_dir(repo, n);
        self.pause(repo, n, &d, false);
        // What last held it no longer does: nothing holds an ended list.
        match fs::remove_file(d.join("held")) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => eprintln!("proj-inbox: couldn't remove {}: {e}", d.display()),
        }
        true
    }

    /// `proj plan end`'s work, under the inbox's pass lock: `caller` is the
    /// GitHub account it runs as. Answers the repo as its clone spells it.
    pub(crate) fn plan_end(&mut self, parent: &str, caller: Option<u64>) -> Result<String, Fail> {
        let (repo, n) = self.plan_door("end", parent, caller)?;
        let n = n.to_string();
        if !self.has_task_list(Some(&repo), &n) {
            return Err(Fail::new(
                1,
                format!("proj plan end: {repo}#{n} has no task list to end"),
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
                format!("proj plan end: couldn't end {repo}#{n}'s task list"),
            ));
        }
        Ok(repo)
    }
}
