//! Durable state writes shared by the reset state and meter.

use std::path::Path;

/// Write FILE whole and to disk: a pass reading it never sees half of it,
/// and a crash just after doesn't lose it.
pub fn write(file: &Path, bytes: &[u8]) -> Result<(), String> {
    burrow_process::replace::replace_mode(file, bytes, burrow_process::replace::SHARED)
        .map_err(|e| format!("{}: {e}", file.display()))
}
