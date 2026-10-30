//! burrow-rc-place: every session of a Remote Control server in a cgroup of
//! its own, under a memory cap of its own (modules/remote-control.nix's
//! claude-rc-scan runs it on every scan).
//!
//! A server is a user unit whose main process is a tmux server. Its pane
//! runs Claude (`claude remote-control`), and each session claude.ai starts
//! there is a child of that Claude, with everything it runs under it. When
//! they all shared the server's one cap, three busy sessions were enough for
//! the kernel to kill one of them. Here each session (`session-PID`) and the
//! server itself (`server`: the tmux and its Claude) has a cap of its own,
//! and only one that grows past it is killed.
//!
//! The unit hands the memory controller down (Delegate=memory) and leaves its
//! main process where systemd put it, in the unit's own cgroup. A cgroup with
//! a process of its own can't hand a controller down, so the first placement
//! moves everything out into `server` and the sessions' groups before
//! turning memory on. After that every process starts in its parent's group,
//! and none in the unit's own again. A session starts in `server`, with the
//! Claude that started it, and moves to its own group at the next scan,
//! which its ~/.claude/sessions/PID.json starts.
//!
//! Reads one line for each running server on stdin, `UNIT MAINPID CAP
//! CGROUP` (CGROUP the unit's cgroup directory, CAP as [`cap::Cap`] reads
//! it), and prints `UNIT PID` for each of its sessions, which is how
//! claude-rc-scan tells whether a stale server is in use.

pub mod cap;
pub mod cgroup;
pub mod place;
pub mod procs;

use std::io;
use std::path::PathBuf;

use cap::Cap;
use cgroup::Applied;
use procs::{Pid, Table};

/// How many looks a server gets in one run when processes keep starting in
/// its unit's own cgroup, which only happens before its first placement.
const ROUNDS: usize = 3;

#[derive(Debug, PartialEq, Eq)]
pub struct Server {
    pub unit: String,
    pub main: Pid,
    pub cap: Cap,
    pub cgroup: PathBuf,
}

/// One server's sessions, and what went wrong placing them.
#[derive(Debug)]
pub struct Placed {
    pub unit: String,
    pub sessions: Vec<Pid>,
    pub problem: Option<String>,
}

/// The servers on stdin, one a line: `UNIT MAINPID CAP CGROUP`.
pub fn parse(input: &str) -> Result<Vec<Server>, String> {
    input
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let fields: Vec<&str> = line.splitn(4, ' ').collect();
            let [unit, main, cap, cgroup] = fields[..] else {
                return Err(format!("not UNIT MAINPID CAP CGROUP: {line:?}"));
            };
            if unit.is_empty() || !cgroup.starts_with('/') {
                return Err(format!("not UNIT MAINPID CAP CGROUP: {line:?}"));
            }
            Ok(Server {
                unit: unit.into(),
                main: main.parse().map_err(|_| format!("not a pid: {main:?}"))?,
                cap: cap.parse()?,
                cgroup: cgroup.into(),
            })
        })
        .collect()
}

/// Places every server's sessions. Fails only when it can't read the
/// process table, and so can't say what the sessions are.
pub fn run(servers: &[Server]) -> io::Result<Vec<Placed>> {
    let mut table = Table::read()?;
    let mut placed = Vec::new();
    for server in servers {
        let mut problem = None;
        for _ in 0..ROUNDS {
            let sessions = table.sessions(server.main);
            match cgroup::apply(&server.cgroup, &table, &sessions, server.cap) {
                Ok(Applied::Done) => {
                    problem = None;
                    break;
                }
                Ok(Applied::Again) => {
                    problem = Some("processes kept starting in the unit's own cgroup".into());
                    table = Table::read()?;
                }
                Err(e) => {
                    problem = Some(e.to_string());
                    break;
                }
            }
        }
        placed.push(Placed {
            unit: server.unit.clone(),
            sessions: table.sessions(server.main),
            problem,
        });
    }
    Ok(placed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_line_for_each_server() {
        let input = "claude-rc@remi.service 4100 8G /sys/fs/cgroup/u/claude-rc@remi.service\n\n\
                     claude-rc@a\\x20b.service 7 infinity /sys/fs/cgroup/u/claude-rc@a\\x20b.service\n";
        assert_eq!(
            parse(input),
            Ok(vec![
                Server {
                    unit: "claude-rc@remi.service".into(),
                    main: 4100,
                    cap: Cap::Bytes(8 << 30),
                    cgroup: "/sys/fs/cgroup/u/claude-rc@remi.service".into(),
                },
                Server {
                    unit: "claude-rc@a\\x20b.service".into(),
                    main: 7,
                    cap: Cap::Infinity,
                    cgroup: "/sys/fs/cgroup/u/claude-rc@a\\x20b.service".into(),
                },
            ])
        );
    }

    #[test]
    fn refuses_a_line_it_cant_read() {
        for line in [
            "claude-rc@remi.service 4100 8G",
            "claude-rc@remi.service x 8G /sys/fs/cgroup/u",
            "claude-rc@remi.service 4100 35% /sys/fs/cgroup/u",
            "claude-rc@remi.service 4100 8G sys/fs/cgroup/u",
            " 4100 8G /sys/fs/cgroup/u",
        ] {
            assert!(parse(line).is_err(), "{line:?}");
        }
    }
}
