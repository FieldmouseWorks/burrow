//! The process table, as /proc says it: whose child each process is, and
//! which have already ended.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;

pub type Pid = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Proc {
    pub ppid: Pid,
    /// A zombie, or on its way out: it has ended, and only waits for its
    /// parent to see.
    pub ended: bool,
}

#[derive(Debug, Default)]
pub struct Table {
    procs: HashMap<Pid, Proc>,
    children: HashMap<Pid, Vec<Pid>>,
}

/// One /proc/PID/stat: `PID (COMM) STATE PPID ...`. COMM may hold spaces and
/// parentheses of its own, so the fields after it start at the last `)`.
pub fn parse_stat(stat: &str) -> Option<(Pid, Proc)> {
    let (head, rest) = stat.rsplit_once(')')?;
    let pid = head.split_once(" (")?.0.parse().ok()?;
    let mut fields = rest.split_ascii_whitespace();
    let state = fields.next()?;
    let ppid = fields.next()?.parse().ok()?;
    let ended = matches!(state, "Z" | "X" | "x");
    Some((pid, Proc { ppid, ended }))
}

impl FromIterator<(Pid, Proc)> for Table {
    fn from_iter<I: IntoIterator<Item = (Pid, Proc)>>(iter: I) -> Table {
        let mut table = Table::default();
        for (pid, proc) in iter {
            table.children.entry(proc.ppid).or_default().push(pid);
            table.procs.insert(pid, proc);
        }
        for children in table.children.values_mut() {
            children.sort_unstable();
        }
        table
    }
}

impl Table {
    /// Every process there is now. One that ends while it's being read is
    /// left out.
    pub fn read() -> io::Result<Table> {
        let mut procs = Vec::new();
        for entry in fs::read_dir("/proc")? {
            let entry = entry?;
            if entry
                .file_name()
                .to_str()
                .and_then(|n| n.parse::<Pid>().ok())
                .is_none()
            {
                continue;
            }
            if let Ok(stat) = fs::read_to_string(entry.path().join("stat"))
                && let Some(proc) = parse_stat(&stat)
            {
                procs.push(proc);
            }
        }
        Ok(procs.into_iter().collect())
    }

    fn children(&self, pid: Pid) -> &[Pid] {
        self.children.get(&pid).map_or(&[], Vec::as_slice)
    }

    /// A server's sessions, given its main process (its tmux): the children
    /// of its Claude (the tmux's child, its pane) that haven't ended.
    pub fn sessions(&self, main: Pid) -> Vec<Pid> {
        let mut sessions: Vec<Pid> = self
            .children(main)
            .iter()
            .flat_map(|&claude| self.children(claude))
            .copied()
            .filter(|pid| self.procs.get(pid).is_some_and(|p| !p.ended))
            .collect();
        sessions.sort_unstable();
        sessions
    }

    /// The session `pid` is part of: itself, or its nearest ancestor among
    /// `sessions`.
    pub fn owner(&self, pid: Pid, sessions: &HashSet<Pid>) -> Option<Pid> {
        let mut at = pid;
        // A walk up the table never takes more steps than it has processes.
        for _ in 0..=self.procs.len() {
            if sessions.contains(&at) {
                return Some(at);
            }
            at = self.procs.get(&at)?.ppid;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alive(ppid: Pid) -> Proc {
        Proc { ppid, ended: false }
    }

    // A tmux (10) with its pane's Claude (11), two sessions (20, 30) and a
    // third that has ended (40); 20 runs a shell (21) running cargo (22).
    fn server() -> Table {
        [
            (1, alive(0)),
            (10, alive(1)),
            (11, alive(10)),
            (20, alive(11)),
            (21, alive(20)),
            (22, alive(21)),
            (30, alive(11)),
            (
                40,
                Proc {
                    ppid: 11,
                    ended: true,
                },
            ),
            (50, alive(1)),
        ]
        .into_iter()
        .collect()
    }

    #[test]
    fn reads_a_stat_whose_command_has_spaces_and_parentheses() {
        let stat = "4242 (claude (rc) x) S 4100 4242 4242 0 -1 4194560 1 0 0 0";
        assert_eq!(parse_stat(stat), Some((4242, alive(4100))));
        assert_eq!(
            parse_stat("7 (sh) Z 6 7 7"),
            Some((
                7,
                Proc {
                    ppid: 6,
                    ended: true
                }
            ))
        );
        assert_eq!(parse_stat("7 sh S 6"), None);
        assert_eq!(parse_stat("x (sh) S 6"), None);
    }

    #[test]
    fn sessions_are_the_claudes_children_still_running() {
        assert_eq!(server().sessions(10), vec![20, 30]);
        // Not a server's tmux: its child has no children.
        assert_eq!(server().sessions(50), Vec::<Pid>::new());
        assert_eq!(server().sessions(99), Vec::<Pid>::new());
    }

    #[test]
    fn a_process_belongs_to_the_session_it_runs_under() {
        let table = server();
        let sessions: HashSet<Pid> = table.sessions(10).into_iter().collect();
        assert_eq!(table.owner(20, &sessions), Some(20));
        assert_eq!(table.owner(22, &sessions), Some(20));
        assert_eq!(table.owner(30, &sessions), Some(30));
        assert_eq!(table.owner(11, &sessions), None);
        assert_eq!(table.owner(10, &sessions), None);
        assert_eq!(table.owner(40, &sessions), None);
        assert_eq!(table.owner(99, &sessions), None);
    }

    #[test]
    fn a_loop_in_the_table_ends_the_walk() {
        let table: Table = [(5, alive(6)), (6, alive(5))].into_iter().collect();
        assert_eq!(table.owner(5, &HashSet::from([7])), None);
    }
}
