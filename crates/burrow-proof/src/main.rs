//! `burrow-proof start|run|end DIR`: a proof run, its folder DIR (lib.rs).
//! The steward's pass runs `start`; the unit it starts runs `run`, then
//! `end`; `run` has each command `enter` its fence. `burrow-proof work
//! CONFIG ID` is a run of the proof producer's, in its worker's unit.

use std::path::Path;
use std::process::ExitCode;

use burrow_process::fence::enter;
use burrow_proof::end::{Ended, end};
use burrow_proof::fence::Cgroups;
use burrow_proof::plugin::Config;
use burrow_proof::run::run;
use burrow_proof::start::start;
use burrow_proof::work::{BROKE, work};

const USAGE: &str = "usage:
  burrow-proof start DIR   start the run DIR/build.json says, in its unit
  burrow-proof run DIR     the unit's own process: each check's command
  burrow-proof end DIR     the unit's ExecStopPost: its worktrees removed, DIR/end.json
  burrow-proof work CONFIG ID   a proof producer's run ID, in its worker's unit: its exit is how it came out
  burrow-proof enter FENCE -- PROGRAM [ARG...]   PROGRAM, inside the cgroup FENCE";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Asked for by each verb that needs it, so a work's failure to find
    // it is the work's own (no verdict), never exit 1, which is a fail.
    let exe = || std::env::current_exe().map_err(|e| format!("which program this is: {e}"));
    let done = match args.as_slice() {
        [word, dir] if word == "start" => {
            exe().and_then(|exe| start(Path::new(dir), &exe, Path::new("systemd-run")))
        }
        [word, dir] if word == "run" => exe()
            .and_then(Cgroups::own)
            .and_then(|mut fences| run(Path::new(dir), &mut fences)),
        [word, dir] if word == "end" => end(Path::new(dir), &Ended::from_env()).map(drop),
        [word, config, id] if word == "work" => {
            let code = Config::read(Path::new(config)).and_then(|cfg| {
                let mut fences = Cgroups::own(exe()?)?;
                Ok(work(cfg.bench(), id, &mut fences))
            });
            return ExitCode::from(code.unwrap_or_else(|why| {
                eprintln!("burrow-proof: {why}");
                BROKE
            }));
        }
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
