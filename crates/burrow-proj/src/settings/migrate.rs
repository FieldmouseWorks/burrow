//! `proj settings migrate`: moves the old roles, models and roles.d files
//! (~/.config/proj) into the settings overlay, once, then renames them aside
//! (NAME.migrated), so nothing can read them again by mistake. Their old
//! line formats are read here and nowhere else.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use burrow_settings::Migration;
use burrow_settings::roles::{self, Key, Offer, Roles, Setups, Tuning};

/// The old files, beside each other in DIR.
const FILES: &[&str] = &[
    "roles",
    "models",
    "roles.d",
    "roles.previous",
    "roles.lock",
    "roles.operation",
];

fn text(path: &Path) -> Result<Option<String>, String> {
    match fs::read_to_string(path) {
        Ok(t) => Ok(Some(t)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !path.is_symlink() => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Each line that says something, with its number.
fn lines(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .map(|(i, l)| (i + 1, l))
}

/// A roles file: `ROLE[@OWNER/REPO] ENGINE [over PCT ENGINE]` a line.
fn routes(path: &Path, text: &str) -> Result<BTreeMap<String, roles::Route>, String> {
    let mut out = BTreeMap::new();
    for (n, line) in lines(text) {
        let at = |e: String| format!("{} line {n}: {line}: {e}", path.display());
        let words: Vec<_> = line.split_whitespace().collect();
        let key = Key::parse(words[0]).map_err(at)?.canonical();
        let route = roles::route(&words[1..]).map_err(at)?;
        if out.insert(key, route).is_some() {
            return Err(at("a second line for this role".into()));
        }
    }
    Ok(out)
}

/// A models file: `ROLE[@OWNER/REPO] ENGINE MODEL|- [EFFORT]` a line.
fn models(path: &Path, text: &str) -> Result<BTreeMap<String, Tuning>, String> {
    let mut out = BTreeMap::new();
    for (n, line) in lines(text) {
        let at = |e: String| format!("{} line {n}: {e}", path.display());
        let (key, engine, model, effort) = match line.split_whitespace().collect::<Vec<_>>()[..] {
            [k, e, m] => (k, e, m, None),
            [k, e, m, f] => (k, e, m, Some(f)),
            _ => {
                return Err(at(
                    "expected ROLE[@OWNER/REPO] ENGINE MODEL|- [EFFORT]".into()
                ));
            }
        };
        if !roles::ENGINES.contains(&engine) {
            return Err(at(format!("unknown engine {engine}")));
        }
        let tuning = Tuning {
            model: (model != "-").then(|| model.to_owned()),
            effort: effort.map(str::to_owned),
        };
        if [&tuning.model, &tuning.effort]
            .into_iter()
            .flatten()
            .any(|w| !roles::plain(w))
        {
            return Err(at("not a plain model or effort".into()));
        }
        let key = format!("{} {engine}", Key::parse(key).map_err(at)?.canonical());
        if out.insert(key, tuning).is_some() {
            return Err(at("a second line for this role and engine".into()));
        }
    }
    Ok(out)
}

/// What the old files in DIR hold, as the overlay will.
pub(crate) fn read(dir: &Path) -> Result<Migration, String> {
    let roles_path = dir.join("roles");
    let models_path = dir.join("models");
    let tuned = match text(&models_path)? {
        Some(t) => models(&models_path, &t)?,
        None => BTreeMap::new(),
    };
    // Each engine offered the models and efforts its roles were set to.
    let mut offers: BTreeMap<String, Offer> = BTreeMap::new();
    for (key, tuning) in &tuned {
        let engine = key.rsplit(' ').next().unwrap_or_default();
        let offer = offers.entry(engine.into()).or_default();
        offer.models.extend(tuning.model.clone());
        offer.efforts.extend(tuning.effort.clone());
    }
    let routing = Roles {
        roles: match text(&roles_path)? {
            Some(t) => routes(&roles_path, &t)?,
            None => BTreeMap::new(),
        },
        models: tuned.clone(),
    };
    // A setup was a roles file alone, the models file applying under each.
    let mut setups = Setups::default();
    let saved = dir.join("roles.d");
    match fs::read_dir(&saved) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry.map_err(|e| format!("{}: {e}", saved.display()))?;
                let name = entry.file_name().to_string_lossy().into_owned();
                if !roles::valid_setup(&name) || !entry.path().is_file() {
                    continue;
                }
                let path = entry.path();
                let t = text(&path)?.unwrap_or_default();
                setups.saved.insert(
                    name,
                    Roles {
                        roles: routes(&path, &t)?,
                        models: tuned.clone(),
                    },
                );
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("{}: {e}", saved.display())),
    }
    // The setup the roles file linked to, as `engines use` left it.
    setups.active = fs::read_link(&roles_path)
        .ok()
        .and_then(|t| t.file_name().map(|n| n.to_string_lossy().into_owned()))
        .filter(|n| setups.saved.get(n) == Some(&routing));
    setups.previous =
        text(&dir.join("roles.previous"))?.and_then(|name| setups.saved.get(name.trim()).cloned());
    Ok(Migration {
        routing,
        offers,
        setups,
    })
}

/// The old files still in DIR.
pub(crate) fn left(dir: &Path) -> Vec<PathBuf> {
    FILES
        .iter()
        .map(|f| dir.join(f))
        .filter(|p| p.symlink_metadata().is_ok())
        .collect()
}

/// Moves the old files in DIR into SETTINGS once, then renames them aside.
pub(crate) fn run(settings: &burrow_settings::Config, dir: &Path) -> Result<String, String> {
    let moved = settings.migrate(read(dir)?, "proj settings migrate")?;
    let old = left(dir);
    if !moved && !old.is_empty() {
        return Err(format!(
            "the settings took the old files once already; {} came back, and nothing reads it: make its changes with proj settings, then remove it",
            old[0].display()
        ));
    }
    for path in &old {
        let mut aside = path.clone().into_os_string();
        aside.push(".migrated");
        fs::rename(path, &aside).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(if moved {
        format!(
            "Moved {} into the settings; proj settings show has them.\n",
            if old.is_empty() {
                "nothing (no old files)".to_owned()
            } else {
                old.iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        )
    } else {
        "The settings took the old files already.\n".into()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_old_files_migrate_once() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("config");
        fs::create_dir_all(dir.join("roles.d")).unwrap();
        fs::write(dir.join("roles.d/cheap"), "triage codex\n").unwrap();
        fs::write(
            dir.join("roles.d/usual"),
            "# usual\ntriage claude\nfix@Sam/Box   codex over 80 claude\n",
        )
        .unwrap();
        std::os::unix::fs::symlink(dir.join("roles.d/usual"), dir.join("roles")).unwrap();
        fs::write(dir.join("roles.previous"), "cheap\n").unwrap();
        fs::write(
            dir.join("models"),
            "triage claude small low\nfix codex - ultra\n",
        )
        .unwrap();
        let settings = burrow_settings::Config {
            overlay: d.path().join("overlay.json"),
            ..Default::default()
        };
        let said = run(&settings, &dir).unwrap();
        assert!(said.starts_with("Moved "), "{said}");
        let force = settings.roles().unwrap();
        assert_eq!(force.route("fix", Some("sam/box")).engine, "codex");
        assert_eq!(force.route("triage", None).engine, "claude");
        assert_eq!(
            force.tuning("triage", None, "claude").model.as_deref(),
            Some("small")
        );
        // Codex's own level, which its default offer lacks, comes with it.
        assert_eq!(
            force.tuning("fix", None, "codex").effort.as_deref(),
            Some("ultra")
        );
        let engines = settings.engines().unwrap();
        assert!(
            engines.contains("Active: usual\nPrevious: cheap\n"),
            "{engines}"
        );
        assert!(left(&dir).is_empty());
        assert!(dir.join("roles.d.migrated/usual").is_file());

        // Once: a file written again later is refused, never read.
        fs::write(dir.join("roles"), "triage codex\n").unwrap();
        let err = run(&settings, &dir).unwrap_err();
        assert!(err.contains("once already"), "{err}");
        assert_eq!(
            settings.roles().unwrap().route("triage", None).engine,
            "claude"
        );
        fs::remove_file(dir.join("roles")).unwrap();
        assert!(run(&settings, &dir).unwrap().contains("already"));
    }

    #[test]
    fn a_bad_old_file_moves_nothing() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("roles"), "# old\ntriage old-model\n").unwrap();
        let settings = burrow_settings::Config {
            overlay: d.path().join("overlay.json"),
            ..Default::default()
        };
        let err = run(&settings, d.path()).unwrap_err();
        assert!(err.contains("roles line 2"), "{err}");
        assert!(!settings.overlay.exists());
        assert!(d.path().join("roles").is_file());
    }
}
