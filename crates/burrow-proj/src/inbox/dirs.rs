//! Local record directories.

use std::fs;
use std::path::{Path, PathBuf};

/// The folders in `dir` whose names start with `prefix`, by name, but never
/// a dot one.
pub(super) fn dirs(dir: &Path, prefix: &str) -> Vec<PathBuf> {
    let mut all: Vec<PathBuf> = fs::read_dir(dir)
        .map(|d| {
            d.filter_map(|e| e.ok())
                .filter(|e| {
                    let name = e.file_name();
                    let name = name.to_string_lossy();
                    !name.starts_with('.') && name.starts_with(prefix)
                })
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default();
    all.sort();
    all
}
