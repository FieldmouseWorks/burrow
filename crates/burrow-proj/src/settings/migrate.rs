//! `proj settings migrate`: moves the old roles, models and roles.d files
//! (~/.config/proj) into the settings, once. Their old line formats are read
//! here and nowhere else. The files stay where they are, so a generation from
//! before the settings still has them after a rollback; nothing since reads
//! them, and proj refuses role settings while they're there unmoved.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::Path;

use crate::flock::Flock;
use burrow_settings::roles::{self, Key, Migration, Offer, Roles, Setups, Tuning};

mod fence;
mod recovery;

#[cfg(test)]
mod regressions;

fn text(path: &Path) -> Result<Option<String>, String> {
    match fs::read_to_string(path) {
        Ok(t) => Ok(Some(t)),
        Err(e)
            if e.kind() == std::io::ErrorKind::NotFound && !burrow_settings::path_exists(path)? =>
        {
            Ok(None)
        }
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

/// The efforts each engine's CLI took, as the old models file checked them:
/// Claude Code's `--effort` has a fixed set; Codex's levels are its models'
/// own, so any plain lowercase word.
fn effort_ok(engine: &str, effort: &str) -> bool {
    match engine {
        "claude" => ["low", "medium", "high", "xhigh", "max"].contains(&effort),
        _ => {
            !effort.is_empty()
                && effort
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(&b))
        }
    }
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
        let model = (model != "-").then(|| model.to_owned());
        if model.as_deref().is_some_and(|m| !roles::plain(m)) {
            return Err(at("invalid model choice".into()));
        }
        if let Some(e) = effort.filter(|e| !effort_ok(engine, e)) {
            return Err(at(format!("{engine} takes no effort {e}")));
        }
        let tuning = Tuning {
            model,
            effort: effort.map(str::to_owned),
        };
        let key = format!("{} {engine}", Key::parse(key).map_err(at)?.canonical());
        if out.insert(key, tuning).is_some() {
            return Err(at("a second line for this role and engine".into()));
        }
    }
    Ok(out)
}

/// What the old files in DIR hold, as the settings will, and the switch
/// they had in flight.
fn read(dir: &Path) -> Result<Migration, String> {
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
                if !roles::valid_setup(&name)
                    || !burrow_settings::path_exists(&entry.path())?
                    || !fs::metadata(entry.path())
                        .map_err(|e| e.to_string())?
                        .is_file()
                {
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
        Err(e)
            if e.kind() == std::io::ErrorKind::NotFound
                && !burrow_settings::path_exists(&saved)? => {}
        Err(e) => return Err(format!("{}: {e}", saved.display())),
    }
    // The setup the roles file linked to, as `engines use` left it.
    setups.active = fs::read_link(&roles_path)
        .ok()
        .and_then(|t| t.file_name().map(|n| n.to_string_lossy().into_owned()))
        .filter(|n| setups.saved.get(n) == Some(&routing));
    let previous = text(&dir.join("roles.previous"))?.map(|n| n.trim().to_owned());
    setups.previous = previous.and_then(|name| setups.saved.get(&name).cloned());
    Ok(Migration {
        routing,
        offers,
        setups,
        receipts: BTreeMap::new(),
    })
}

fn lock(path: &Path) -> Result<Flock, String> {
    fs::create_dir_all(path.parent().ok_or("lock has no parent")?).map_err(|e| e.to_string())?;
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    file.lock()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    std::mem::forget(Flock::held(file.try_clone().unwrap()));
    Ok(Flock::held(file))
}

/// Both locks remain held for the deployed generation with --fence. A retained
/// door cannot finish a fetched request, nor can a CLI waiting on roles.lock
/// mutate retired inputs. Stopping this generation releases them for rollback.
pub(crate) fn run(settings: &burrow_settings::Config, dir: &Path) -> Result<String, String> {
    if settings.roles_migrated()? {
        settings.migrate(Migration::default(), "proj settings migrate")?;
        return Ok("The settings took the old files already.\n".into());
    }
    run_with(settings, dir, |_, _| Ok(()))
}

fn run_with(
    settings: &burrow_settings::Config,
    dir: &Path,
    after: impl FnOnce(&Result<String, String>, &[&Flock]) -> Result<(), String>,
) -> Result<String, String> {
    let door = settings
        .legacy_engines_lock
        .as_deref()
        .map(lock)
        .transpose()?;
    let roles = lock(&dir.join("roles.lock"))?;
    let answer = migrate_locked(settings, dir);
    // A failed migration must not release the retired writers. The deployed
    // fence stays up through refusal as well as success; health separately
    // checks the durable cutover before a deployment can be confirmed.
    let held: Vec<&Flock> = door.iter().chain([&roles]).collect();
    after(&answer, &held)?;
    answer
}

fn migrate_locked(settings: &burrow_settings::Config, dir: &Path) -> Result<String, String> {
    if settings.roles_migrated()? {
        settings.migrate(Migration::default(), "proj settings migrate")?;
        Ok("The settings took the old files already.\n".into())
    } else {
        let receipts = recovery::run(dir)?;
        let mut from = read(dir)?;
        from.receipts = receipts;
        settings.migrate(from, "proj settings migrate")?;
        Ok(format!(
            "Moved what {} held into the settings; proj settings show has them. Nothing reads the old files now.\n",
            dir.display()
        ))
    }
}

pub(crate) fn fenced(settings: &burrow_settings::Config, dir: &Path) -> Result<String, String> {
    run_with(settings, dir, |answer, _| {
        if let Err(error) = answer {
            eprintln!("proj: migration failed; retained writers stay fenced: {error}");
        }
        fence::hold()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(d: &Path) -> burrow_settings::Config {
        burrow_settings::Config {
            overlay: d.join("settings/overlay.json"),
            ..Default::default()
        }
    }

    #[test]
    fn the_old_files_migrate_once() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("config");
        fs::create_dir_all(dir.join("roles.d")).unwrap();
        fs::write(dir.join("roles.d/cheap"), "triage codex\n").unwrap();
        fs::write(
            dir.join("roles.d/usual"),
            "# usual\ntriage claude\nfix@Sam/Box   codex over 80.5 claude\n",
        )
        .unwrap();
        std::os::unix::fs::symlink(dir.join("roles.d/usual"), dir.join("roles")).unwrap();
        fs::write(dir.join("roles.previous"), "cheap\n").unwrap();
        fs::write(
            dir.join("models"),
            "triage claude small low\nfix codex - ultra\n",
        )
        .unwrap();
        let settings = settings(d.path());
        // proj won't read role settings until they're moved in.
        let mut cfg: crate::config::Config =
            serde_json::from_value(crate::config::tests::example()).unwrap();
        cfg.settings = Some(settings.clone());
        cfg.old_config = Some(dir.clone());
        let err = cfg.check_config().unwrap_err();
        assert!(err.contains("aren't moved into the settings yet"), "{err}");

        let said = run(&settings, &dir).unwrap();
        assert!(said.starts_with("Moved "), "{said}");
        cfg.check_config().unwrap();
        let force = settings.roles().unwrap();
        let fix = force.route("fix", Some("sam/box"));
        assert_eq!(fix.engine, "codex");
        assert_eq!(fix.over.as_ref().unwrap().pct, 80.5, "fractions kept");
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
        // The old files stay for the generation before, unread since.
        assert!(dir.join("roles.d/usual").is_file());
        fs::write(dir.join("models"), "triage claude other\n").unwrap();
        assert!(run(&settings, &dir).unwrap().contains("already"));
        assert_eq!(
            settings
                .roles()
                .unwrap()
                .tuning("triage", None, "claude")
                .model
                .as_deref(),
            Some("small")
        );
        let log = fs::read_to_string(settings.overlay.with_extension("log")).unwrap();
        assert_eq!(log.lines().count(), 1, "{log}");
    }

    #[test]
    fn a_bad_old_file_moves_nothing() {
        for (file, text, why) in [
            ("roles", "# old\ntriage old-model\n", "roles line 2"),
            (
                "models",
                "# old\nunused claude - enormous\n",
                "claude takes no effort enormous",
            ),
            (
                "models",
                "unused codex - Max\n",
                "codex takes no effort Max",
            ),
            ("models", "unused claude a$b\n", "invalid model choice"),
            ("roles", "work claude over 100.5 codex\n", "roles line 1"),
        ] {
            let d = tempfile::tempdir().unwrap();
            fs::write(d.path().join(file), text).unwrap();
            let settings = settings(d.path());
            let err = run(&settings, d.path()).unwrap_err();
            assert!(err.contains(why), "{text}: {err}");
            assert!(!settings.roles_file().exists());
        }
    }

    #[test]
    fn a_journalled_switch_is_made_and_answered_once() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("config");
        fs::create_dir_all(dir.join("roles.d")).unwrap();
        fs::write(dir.join("roles.d/one"), "work claude\n").unwrap();
        fs::write(dir.join("roles.d/two"), "work codex\n").unwrap();
        std::os::unix::fs::symlink(dir.join("roles.d/one"), dir.join("roles")).unwrap();
        let receipt = d.path().join("door/1-cycle.operation");
        fs::create_dir_all(receipt.parent().unwrap()).unwrap();
        let op = serde_json::json!({"receipt": receipt, "destination": "two", "before": "one", "result": null});
        fs::write(dir.join("roles.operation"), op.to_string()).unwrap();
        let settings = settings(d.path());
        run(&settings, &dir).unwrap();
        assert_eq!(
            settings.roles().unwrap().route("work", None).engine,
            "codex"
        );
        let engines = settings.engines().unwrap();
        assert!(
            engines.contains("Active: two\nPrevious: one\n"),
            "{engines}"
        );
        // Where the old door looks for its answer.
        let done: serde_json::Value = serde_json::from_slice(&fs::read(&receipt).unwrap()).unwrap();
        assert_eq!(done["result"], "Was active: one\nActive: two\n");
    }
}
