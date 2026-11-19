//! Where an issue stands on the task lists, read from them alone: what an
//! issue's record ([`crate::inbox::record`]) says of a planned parent and
//! its children, and the part of a child's admission
//! ([`Pass::child_admissible`]) that needs nothing but the lists.

use std::fs;

use super::{List, Pass};

/// Where an issue stands on the task lists ([`Pass::place`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::inbox) enum Place {
    /// A split parent: the child its list waits on now, None once every
    /// child has landed.
    Parent(Option<String>),
    /// A child its list doesn't hand on yet: the one before it that hasn't
    /// landed, its own list's or an ancestor's.
    Behind(String),
    /// A child whose parent's own job (this one) is still at it.
    ParentJob(String),
    /// A child its list has landed already.
    Landed(String),
    /// A child its parent, OWNER/REPO#P, was replanned without.
    Superseded(String),
    /// A plan that can't go on as it stands, so the child doesn't either:
    /// what's wrong, in words (a task list that doesn't parse holds every
    /// child in its repo).
    Stuck(String),
}

impl Pass<'_> {
    /// Where issue N stands on the task lists, from the lists alone (no
    /// GitHub, nothing written): a parent with a list of its own
    /// ([`Place::Parent`]) waits on its next child, and a child on what
    /// [`Pass::plan_hold`] says holds it. None when no list has it, or its
    /// list hands it on next.
    pub(in crate::inbox) fn place(&self, repo: &str, n: &str) -> Option<Place> {
        if !self.inbox.planning {
            return None;
        }
        if self.has_task_list(Some(repo), n) {
            let list: Option<List> = fs::read(self.plan_dir(repo, n).join("tasks.json"))
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok());
            return Some(Place::Parent(
                list.as_ref().and_then(|l| Some(child(repo, l, l.next()?))),
            ));
        }
        self.plan_hold(repo, n)
    }

    /// What, on the task lists alone, keeps issue N from being admitted as
    /// a child: a list that can't be read, its list's earlier child, its
    /// parent's own live job, its having landed, its parent's replan
    /// without it, and the same of each ancestor. [`Pass::child_admissible`]
    /// refuses whatever this names, then checks the rest on GitHub.
    pub(in crate::inbox) fn plan_hold(&self, repo: &str, n: &str) -> Option<Place> {
        self.plan_hold_at(repo, n, 0)
    }

    fn plan_hold_at(&self, repo: &str, n: &str, depth: usize) -> Option<Place> {
        if !self.inbox.planning {
            return None;
        }
        if depth >= 64 {
            return Some(Place::Stuck("its plans nest more than 64 deep".into()));
        }
        for d in self.plan_dirs(repo) {
            let Ok(bytes) = fs::read(d.join("tasks.json")) else {
                continue;
            };
            let parent = d
                .file_name()
                .and_then(|s| s.to_str())
                .and_then(|s| s.rsplit_once('-').map(|(_, p)| p.to_owned()));
            let named = || format!("{repo}#{}", parent.as_deref().unwrap_or("?"));
            let Ok(list) = serde_json::from_slice::<List>(&bytes) else {
                return Some(Place::Stuck(format!(
                    "{}'s task list doesn't parse",
                    named()
                )));
            };
            let at = |c: &super::Child| c.number.map(|v| v.to_string()).as_deref() == Some(n);
            let Some(index) = list.children.iter().position(at) else {
                if list.retained.iter().any(|old| at(&old.child)) {
                    return Some(Place::Superseded(named()));
                }
                continue;
            };
            let Some(parent) = parent.as_deref() else {
                return Some(Place::Stuck(format!(
                    "the task list at {} isn't named for its parent",
                    d.display()
                )));
            };
            if let Some(id) = self.owner(repo, parent) {
                return Some(Place::ParentJob(id));
            }
            match list.next() {
                Some(next) if next == index => {}
                Some(next) if next < index => return Some(Place::Behind(child(repo, &list, next))),
                _ => return Some(Place::Landed(named())),
            }
            if let Some(held) = self.plan_hold_at(repo, parent, depth + 1) {
                return Some(match held {
                    // A parent that's itself a split child holds its
                    // children as it's held, and one its own plan has
                    // landed or dropped holds them for good.
                    Place::Landed(_) | Place::Superseded(_) => Place::Stuck(format!(
                        "its parent {} is no longer a step its own plan hands on",
                        named()
                    )),
                    held => held,
                });
            }
        }
        None
    }
}

/// Child `i` of `list`, as its record names it.
fn child(repo: &str, list: &List, i: usize) -> String {
    list.children[i].number.map_or_else(
        || format!("its child {} (not opened yet)", i + 1),
        |c| format!("{repo}#{c}"),
    )
}
