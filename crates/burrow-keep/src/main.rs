//! burrow-keep DIR, as flake-check.yml's keep step runs it: DIR is the
//! runner's temp directory, with the kept builds restored to DIR/kept, and
//! what this push keeps is left there.

use std::env;
use std::path::Path;
use std::process::ExitCode;

use burrow_keep::{keep, nix::Real};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let [dir] = args.as_slice() else {
        eprintln!("usage: burrow-keep DIR");
        return ExitCode::FAILURE;
    };
    match keep(&Real, Path::new(dir)) {
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
