//! Guest-only turn declarations, installed as proj in the sandbox image.
use std::{env, path::PathBuf, process::ExitCode};

fn main() -> ExitCode {
    let result = env::var_os("PROJ_VM_NEXT")
        .ok_or_else(|| "proj is for a sandboxed proj job".to_string())
        .and_then(|p| {
            burrow_proj::guest::declare(&PathBuf::from(p), &env::args().skip(1).collect::<Vec<_>>())
        });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("proj: {e}");
            ExitCode::FAILURE
        }
    }
}
