//! One file parser for turns, checked switches and deployment checks.
use crate::engine::{Engine, RoleKey, role};
use std::collections::BTreeMap;
use std::path::Path;

pub(crate) fn read(path: &Path) -> Result<String, String> {
    match std::fs::read_to_string(path) {
        Ok(t) => Ok(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !path.is_symlink() => {
            Ok(String::new())
        }
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

pub(crate) fn parse<'a>(
    path: &Path,
    text: &'a str,
    meters: Option<&Path>,
    held: bool,
    now: i64,
) -> Result<BTreeMap<RoleKey<'a>, Engine>, String> {
    let mut entries = BTreeMap::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let (key, engine) = role(line, meters, held, now)
            .map_err(|e| format!("{} line {}: {line}: {e}", path.display(), i + 1))?;
        if let Some((first, original, _)) = entries.get(&key) {
            return Err(format!(
                "{}: duplicate role key: line {first}: {original}; line {}: {line}",
                path.display(),
                i + 1
            ));
        }
        entries.insert(key, (i + 1, line, engine));
    }
    Ok(entries
        .into_iter()
        .map(|(key, (_, _, engine))| (key, engine))
        .collect())
}

/// A validated policy, one line a role, as the shared parser read it.
pub(crate) fn engines(path: &Path, text: &str) -> Result<String, String> {
    parse(path, text, None, false, crate::now())?;
    Ok(text
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" ") + "\n")
        .collect())
}
