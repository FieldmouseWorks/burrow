//! Durable state writes shared by the reset state and meter.

use std::sync::atomic::{AtomicU64, Ordering};
use std::{fs, path::Path};

/// Write FILE whole and to disk: a pass reading it never sees half of it,
/// concurrent writers (usage-reset and claude-login sharing meters/claude.json)
/// never share a temp path, and a crash just after doesn't lose it.
pub fn write(file: &Path, bytes: &[u8]) -> Result<(), String> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = file
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    let tmp = dir.join(format!(
        ".{name}.tmp.{}.{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let result = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .and_then(|mut f| {
            std::io::Write::write_all(&mut f, bytes)?;
            f.sync_all()
        })
        .and_then(|()| fs::rename(&tmp, file))
        .and_then(|()| fs::File::open(dir)?.sync_all());
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.map_err(|e| format!("{}: {e}", file.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_writes_never_race_or_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("claude.json");
        std::thread::scope(|scope| {
            for writer in 0..8 {
                let file = &file;
                scope.spawn(move || {
                    let payload =
                        format!("{{\"writer\":{writer},\"pad\":\"{}\"}}\n", "x".repeat(256));
                    for _ in 0..25 {
                        write(file, payload.as_bytes()).unwrap();
                        let seen = fs::read_to_string(file).unwrap();
                        let parsed: serde_json::Value = serde_json::from_str(&seen).unwrap();
                        assert!(parsed["writer"].is_u64(), "unexpected content: {seen}");
                    }
                });
            }
        });
        let entries: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from("claude.json")]);
    }
}
