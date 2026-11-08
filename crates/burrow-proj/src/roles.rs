//! One file parser for turns, checked switches and deployment checks.
use crate::engine::{Choice, RoleKey, role};
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
) -> Result<BTreeMap<RoleKey<'a>, Choice>, String> {
    let mut entries = BTreeMap::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let (key, choice) = role(line, meters, held, now)
            .map_err(|e| format!("{} line {}: {line}: {e}", path.display(), i + 1))?;
        if let Some((first, original, _)) = entries.get(&key) {
            return Err(format!(
                "{}: duplicate role key: line {first}: {original}; line {}: {line}",
                path.display(),
                i + 1
            ));
        }
        entries.insert(key, (i + 1, line, choice));
    }
    Ok(entries
        .into_iter()
        .map(|(key, (_, _, choice))| (key, choice))
        .collect())
}

/// A validated policy with both primary and overflow engines, without models.
pub(crate) fn engines(path: &Path, text: &str) -> Result<String, String> {
    parse(path, text, None, false, crate::now())?;
    let mut out = String::new();
    for line in text
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
    {
        // The shared parser established the grammar. Only the two engine
        // positions can contain model choices; no raw line is ever printed.
        let words: Vec<_> = line
            .split_whitespace()
            .enumerate()
            .map(|(i, word)| {
                if i == 1 || i == 4 {
                    word.split(':').next().unwrap_or("")
                } else {
                    word
                }
            })
            .collect();
        out += &words.join(" ");
        out.push('\n');
    }
    Ok(out)
}
