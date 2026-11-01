//! `burrow-proof start|run|end DIR`: a proof run, its folder DIR (lib.rs).
//! The steward's pass runs `start`; the unit it starts runs `run`, then
//! `end`; `run` has each command `enter` its fence.

use std::path::Path;
use std::process::ExitCode;

use burrow_proof::end::{Ended, end};
use burrow_proof::fence::{Cgroups, enter};
use burrow_proof::run::run;
use burrow_proof::start::start;

const USAGE: &str = "usage:
  burrow-proof start DIR   start the run DIR/build.json says, in its unit
  burrow-proof run DIR     the unit's own process: each check's command
  burrow-proof end DIR     the unit's ExecStopPost: its worktrees removed, DIR/end.json
  burrow-proof enter FENCE -- PROGRAM [ARG...]   PROGRAM, inside the cgroup FENCE";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => return failed(&format!("which program this is: {e}")),
    };
    let done = match args.as_slice() {
        [word, dir] if word == "start" => start(Path::new(dir), &exe, Path::new("systemd-run")),
        [word, dir] if word == "run" => {
            Cgroups::own(exe).and_then(|mut fences| run(Path::new(dir), &mut fences))
        }
        [word, dir] if word == "end" => end(Path::new(dir), &Ended::from_env()).map(drop),
        [word, fence, dashes, program, rest @ ..] if word == "enter" && dashes == "--" => {
            let e = enter(Path::new(fence), program, rest);
            eprintln!("burrow-proof: enter {fence}: {program}: {e}");
            // What a shell says of a command it can't run.
            return ExitCode::from(126);
        }
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match done {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => failed(&why),
    }
}

fn failed(why: &str) -> ExitCode {
    eprintln!("burrow-proof: {why}");
    ExitCode::FAILURE
}
