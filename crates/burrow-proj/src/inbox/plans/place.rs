//! Where an issue stands on the task lists, read from them alone: what an
//! issue's record ([`crate::inbox::record`]) says of a planned parent and
//! its children.

use std::fs;

use super::{List, Pass};

/// Where an issue stands on the task lists ([`Pass::place`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::inbox) enum Place {
    /// A split parent: the child its list waits on now, None once every
    /// child has landed.
    Parent(Option<String>),
    /// A child its list doesn't hand on yet: the one before it that hasn't
    /// landed.
    Behind(String),
}
impl Pass<'_> {
    /// Where issue N stands on the task lists, from the lists alone (no
    /// GitHub, nothing written): a parent with a list of its own
    /// ([`Place::Parent`]) waits on its next child, and a child that isn't
    /// its list's next ([`Place::Behind`]) on the one before it that hasn't
    /// landed. None when no list has it, or its list hands it on next.
    pub(in crate::inbox) fn place(&self, repo: &str, n: &str) -> Option<Place> {
        if !self.inbox.planning {
            return None;
        }
        let child = |list: &List, i: usize| {
            list.children[i].number.map_or_else(
                || format!("its child {} (not opened yet)", i + 1),
                |c| format!("{repo}#{c}"),
            )
        };
        if self.has_task_list(Some(repo), n) {
            let list: Option<List> = fs::read(self.plan_dir(repo, n).join("tasks.json"))
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok());
            return Some(Place::Parent(
                list.as_ref().and_then(|l| Some(child(l, l.next()?))),
            ));
        }
        self.plan_dirs(repo).iter().find_map(|d| {
            let list: List = serde_json::from_slice(&fs::read(d.join("tasks.json")).ok()?).ok()?;
            let index = list
                .children
                .iter()
                .position(|c| c.number.map(|v| v.to_string()).as_deref() == Some(n))?;
            let next = list.next()?;
            (next < index).then(|| Place::Behind(child(&list, next)))
        })
    }
}
