//! Where each of a server's processes goes: a plan made from what its cgroup
//! holds and the process table, which cgroup.rs carries out.

use std::collections::{BTreeSet, HashSet};

use crate::procs::{Pid, Table};

/// A group under a server's cgroup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Group {
    /// The server itself: its tmux, its Claude, and whatever isn't a
    /// session's.
    Server,
    /// One session, with everything it runs.
    Session(Pid),
}

impl Group {
    pub fn name(self) -> String {
        match self {
            Group::Server => "server".into(),
            Group::Session(pid) => format!("session-{pid}"),
        }
    }

    /// The group a cgroup's name is, if it is one of ours, written the way
    /// [`Group::name`] writes it.
    pub fn parse(name: &str) -> Option<Group> {
        if name == "server" {
            return Some(Group::Server);
        }
        let group = Group::Session(name.strip_prefix("session-")?.parse().ok()?);
        (group.name() == name).then_some(group)
    }
}

/// What a server's cgroup holds now.
#[derive(Debug, Default)]
pub struct Unit {
    /// The processes in the unit's own cgroup: every one of them, until the
    /// first placement.
    pub root: Vec<Pid>,
    /// The processes in `server`.
    pub server: Vec<Pid>,
    /// The names of the groups under it: ours, and systemd's `.control`
    /// while the unit stops.
    pub groups: Vec<String>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// The groups to have, each under the cap.
    pub groups: BTreeSet<Group>,
    /// Which process goes to which group.
    pub moves: Vec<(Pid, Group)>,
    /// The groups of sessions that have ended, to remove once nothing is
    /// left in them.
    pub ended: Vec<Group>,
}

/// The plan for a server whose sessions are `sessions`. A process in the
/// unit's own cgroup or in `server` that is part of a session goes to that
/// session's group, and anything else in the unit's own goes to `server`. A
/// session's group keeps whatever is in it, a tool its session left running
/// included, and so does a group that isn't ours.
pub fn plan(unit: &Unit, table: &Table, sessions: &[Pid]) -> Plan {
    let live: HashSet<Pid> = sessions.iter().copied().collect();
    let mut plan = Plan::default();
    plan.groups.insert(Group::Server);
    for name in &unit.groups {
        match Group::parse(name) {
            Some(group @ Group::Session(pid)) if live.contains(&pid) => {
                plan.groups.insert(group);
            }
            Some(group @ Group::Session(_)) => plan.ended.push(group),
            _ => {}
        }
    }
    let root = unit.root.iter().map(|&pid| (pid, true));
    let server = unit.server.iter().map(|&pid| (pid, false));
    for (pid, in_root) in root.chain(server) {
        let to = match table.owner(pid, &live) {
            Some(session) => Group::Session(session),
            None if in_root => Group::Server,
            None => continue,
        };
        plan.groups.insert(to);
        plan.moves.push((pid, to));
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procs::Proc;

    // A tmux (10) with its pane's Claude (11) and two sessions (20, 30); 20
    // runs a shell (21) running cargo (22).
    fn table() -> Table {
        [(10, 1), (11, 10), (20, 11), (21, 20), (22, 21), (30, 11)]
            .into_iter()
            .map(|(pid, ppid)| (pid, Proc { ppid, ended: false }))
            .collect()
    }

    fn names(groups: &[&str]) -> Vec<String> {
        groups.iter().map(|g| g.to_string()).collect()
    }

    #[test]
    fn names_read_back_and_nothing_else_does() {
        for group in [Group::Server, Group::Session(4242)] {
            assert_eq!(Group::parse(&group.name()), Some(group));
        }
        for name in [
            ".control",
            "session-",
            "session-+5",
            "session-007",
            "session-x",
            "servers",
        ] {
            assert_eq!(Group::parse(name), None, "{name}");
        }
    }

    #[test]
    fn the_first_placement_empties_the_units_own_cgroup() {
        let unit = Unit {
            root: vec![10, 11, 20, 21, 22, 30],
            ..Unit::default()
        };
        let plan = plan(&unit, &table(), &[20, 30]);
        assert_eq!(
            plan.groups,
            BTreeSet::from([Group::Server, Group::Session(20), Group::Session(30)])
        );
        assert_eq!(
            plan.moves,
            vec![
                (10, Group::Server),
                (11, Group::Server),
                (20, Group::Session(20)),
                (21, Group::Session(20)),
                (22, Group::Session(20)),
                (30, Group::Session(30)),
            ]
        );
        assert!(plan.ended.is_empty());
    }

    #[test]
    fn a_new_session_leaves_the_server_and_the_server_stays() {
        let unit = Unit {
            server: vec![10, 11, 30],
            groups: names(&["server", "session-20"]),
            ..Unit::default()
        };
        let plan = plan(&unit, &table(), &[20, 30]);
        assert_eq!(plan.moves, vec![(30, Group::Session(30))]);
        assert!(plan.groups.contains(&Group::Session(20)));
    }

    #[test]
    fn an_ended_sessions_group_goes_and_anything_not_ours_stays() {
        let unit = Unit {
            server: vec![10, 11],
            groups: names(&["server", "session-20", "session-77", ".control"]),
            ..Unit::default()
        };
        let plan = plan(&unit, &table(), &[20, 30]);
        assert_eq!(plan.ended, vec![Group::Session(77)]);
        assert_eq!(
            plan.groups,
            BTreeSet::from([Group::Server, Group::Session(20)])
        );
        assert!(plan.moves.is_empty());
    }

    #[test]
    fn a_process_with_no_session_stays_where_it_is_or_joins_the_server() {
        // 99 isn't in the table: it ended, or its parent did and it was
        // taken in outside the server.
        let unit = Unit {
            root: vec![99],
            server: vec![98],
            ..Unit::default()
        };
        let plan = plan(&unit, &table(), &[20, 30]);
        assert_eq!(plan.moves, vec![(99, Group::Server)]);
    }
}
