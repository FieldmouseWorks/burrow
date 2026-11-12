//! burrow-keep --gate OUTCOME DIR, as flake-check.yml's keep step runs it:
//! OUTCOME is the gate step's (success or failure), DIR is the runner's temp
//! directory, with the kept builds restored to DIR/kept, and what this push
//! keeps is left there.

use std::env;
use std::path::Path;
use std::process::ExitCode;

use burrow_keep::{Gate, keep, nix::Real};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let (gate, dir) = match args.as_slice() {
        [flag, outcome, dir] if flag == "--gate" => match Gate::from_outcome(outcome) {
            Some(gate) => (gate, dir),
            None => {
                eprintln!("burrow-keep: the gate's outcome is success or failure, not {outcome}");
                return ExitCode::FAILURE;
            }
        },
        _ => {
            eprintln!("usage: burrow-keep --gate success|failure DIR");
            return ExitCode::FAILURE;
        }
    };
    match keep(&Real, Path::new(dir), gate) {
        Ok(k) => {
            println!(
                "kept {} paths from this gate and {} from the kept builds, {} MiB; left out {} downloads, {} MiB",
                k.gate,
                k.carried,
                k.bytes.div_ceil(1 << 20),
                k.downloads,
                k.download_bytes.div_ceil(1 << 20)
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("burrow-keep: {e}");
            ExitCode::FAILURE
        }
    }
}
