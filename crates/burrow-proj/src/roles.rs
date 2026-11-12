//! The roles grammar, run over the overlay's roles as lines: one parser for
//! turns and deployment checks.
use crate::engine::{Engine, RoleKey, role};
use std::collections::BTreeMap;
use std::path::Path;

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
