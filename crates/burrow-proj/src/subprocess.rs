//! Bounded guest transport, using the same subprocess bounds as GitHub.
use std::{process::Command, time::Instant};

pub(crate) fn output(
    command: &mut Command,
    until: Instant,
    max_bytes: usize,
) -> Result<Vec<u8>, String> {
    let out = burrow_process::output(command, None, until, max_bytes)?;
    if !out.status.success() {
        return Err(format!("command failed: {}", out.status));
    }
    Ok(out.stdout)
}
