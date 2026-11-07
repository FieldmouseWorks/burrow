//! Durable state writes shared by the reset state and meter.

use std::{fs, path::Path};

/// Write FILE whole and to disk: a pass reading it never sees half of it,
/// and a crash just after doesn't lose it.
pub fn write(file: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = file.with_extension("tmp");
    let dir = file.parent().unwrap_or(Path::new("."));
    fs::File::create(&tmp)
        .and_then(|mut f| {
            std::io::Write::write_all(&mut f, bytes)?;
            f.sync_all()
        })
        .and_then(|()| fs::rename(&tmp, file))
        .and_then(|()| fs::File::open(dir)?.sync_all())
        .map_err(|e| format!("{}: {e}", file.display()))
}
