//! burrow-rc-place, as claude-rc-scan runs it: the running servers on stdin,
//! their sessions on stdout (lib.rs says how). Exits 1 when it can't say
//! what the sessions are, and 2 when it said but couldn't place some.

use std::io::{self, Read, Write};
use std::process::ExitCode;

use burrow_rc_place::{Placed, parse, run};

fn main() -> ExitCode {
    let mut input = String::new();
    let placed = io::stdin()
        .read_to_string(&mut input)
        .map_err(|e| format!("stdin: {e}"))
        .and_then(|_| parse(&input))
        .and_then(|servers| run(&servers).map_err(|e| format!("/proc: {e}")))
        .and_then(|placed| {
            report(&placed)
                .map_err(|e| format!("stdout: {e}"))
                .map(|()| placed)
        });
    match placed {
        Err(e) => {
            eprintln!("burrow-rc-place: {e}");
            ExitCode::from(1)
        }
        Ok(placed) => {
            let mut failed = false;
            for Placed { unit, problem, .. } in &placed {
                if let Some(problem) = problem {
                    eprintln!("burrow-rc-place: {unit}: {problem}");
                    failed = true;
                }
            }
            if failed {
                ExitCode::from(2)
            } else {
                ExitCode::SUCCESS
            }
        }
    }
}

fn report(placed: &[Placed]) -> io::Result<()> {
    let mut out = io::stdout().lock();
    for Placed { unit, sessions, .. } in placed {
        for session in sessions {
            writeln!(out, "{unit} {session}")?;
        }
    }
    out.flush()
}
