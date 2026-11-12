//! The inbox's part of the live snapshot ([`crate::snapshot`]): the ready
//! issues in the drain's order, and the drain's and sorting's slots, each
//! from the same records and ranking the passes act on. Only reads: no
//! GitHub (an observing pass, [`Pass::observe`]), no question recovered.

use std::collections::BTreeMap;
use std::path::Path;

use jiff::Timestamp;

use super::drain::Candidate;
use super::{Pass, dirs};
use crate::config::{Config, Triage};
use crate::snapshot::{Holder, Ready, Slot, SlotKind, SlotState, held, job_slot};
use crate::{mtime, read};

#[derive(Default)]
pub(crate) struct View {
    pub(crate) queue: Vec<Ready>,
    pub(crate) slots: Vec<Slot>,
}

/// What the inbox has at `at`; nothing when its triage is off.
pub(crate) fn view(cfg: &Config, at: i64) -> View {
    let Some(inbox) = cfg.inbox.as_ref() else {
        return View::default();
    };
    let Some(t) = inbox.triage.as_ref() else {
        return View::default();
    };
    let mut pass = Pass {
        cfg,
        inbox,
        project: super::projects(cfg),
        wake: BTreeMap::new(),
        unreached: Vec::new(),
        looked: Vec::new(),
        observe: true,
    };
    pass.view(t, at)
}

impl Pass<'_> {
    pub(super) fn view(&mut self, t: &Triage, at: i64) -> View {
        let mut view = View::default();
        if t.drain.is_some() {
            view.queue = self
                .next_ready(t)
                .into_iter()
                .map(|c| ready(c, at))
                .collect();
            let records = self.drain_records();
            view.slots
                .extend(self.slots(SlotKind::Drain, t.drain_jobs, &records, "drained", at));
        }
        let mut sorting: Vec<_> = dirs(&self.sorting_dir(), "")
            .into_iter()
            .filter(|d| self.holds_sorting_slot(d, false))
            .collect();
        sorting.sort_by_key(|d| (mtime(&d.join("job")).unwrap_or(0), d.clone()));
        view.slots
            .extend(self.slots(SlotKind::Sorting, t.at_once, &sorting, "job", at));
        view
    }

    /// `room` slots of `kind`, each record holding one in turn (more when
    /// more hold them: repair children go past the cap), the rest idle. A
    /// record's `mark` file says since when it has held its slot while its
    /// job hasn't started.
    fn slots(
        &self,
        kind: SlotKind,
        room: usize,
        records: &[std::path::PathBuf],
        mark: &str,
        at: i64,
    ) -> Vec<Slot> {
        (0..room.max(records.len()))
            .map(|i| {
                let name = (i + 1).to_string();
                let Some(d) = records.get(i) else {
                    return held(kind, name, SlotState::Idle, None, at);
                };
                let issue = record_issue(d);
                match read(&d.join("job")).filter(|id| !id.is_empty()) {
                    Some(id) => job_slot(self.cfg, kind, name, &id, issue, at),
                    None => Slot {
                        holder: Some(Holder {
                            issue,
                            ..Holder::default()
                        }),
                        ..held(kind, name, SlotState::Starting, mtime(&d.join(mark)), at)
                    },
                }
            })
            .collect()
    }
}

/// The issue a record is for, OWNER/REPO#N, from its folder's name
/// (OWNER_REPO-N: a GitHub owner has no _ in it).
fn record_issue(d: &Path) -> Option<String> {
    let name = d.file_name()?.to_string_lossy().into_owned();
    let (repo, n) = name.rsplit_once('-')?;
    let (owner, repo) = repo.split_once('_')?;
    Some(format!("{owner}/{repo}#{n}"))
}

fn ready(c: Candidate, at: i64) -> Ready {
    Ready {
        number: c.n.parse().unwrap_or_default(),
        priority: c.priority,
        pipeline: !c.outside,
        repair: c.repair,
        age: c
            .opened
            .parse::<Timestamp>()
            .ok()
            .map(|t| (at - t.as_second()).max(0)),
        opened: c.opened,
        repo: c.repo,
    }
}
