//! The engine, model and effort each role runs on, and the named sets of them
//! (`proj engines save/use/back`): live settings in the overlay, written only
//! through the settings door and logged with the rest of its changes.
//!
//! A model is accepted only if it was first offered for its engine
//! (`offer ENGINE MODEL...`), so a typo never silently runs a role on a model
//! nothing serves. Model ids live in the overlay, never in a tracked file.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const ENGINES: [&str; 2] = ["claude", "codex"];

/// A role's model and effort on one engine; None is the engine's own.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Tuning {
    pub model: Option<String>,
    pub effort: Option<String>,
}

/// One saved set: exactly the values `engines use` puts in force.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Saved {
    pub roles: BTreeMap<String, String>,
    pub models: BTreeMap<String, Tuning>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Engines {
    /// `ROLE[@OWNER/REPO]` to `ENGINE [over PCT ENGINE]`.
    #[serde(default)]
    pub roles: BTreeMap<String, String>,
    /// `ROLE[@OWNER/REPO] ENGINE` to its tuning.
    #[serde(default)]
    pub models: BTreeMap<String, Tuning>,
    /// The models each engine offers.
    #[serde(default)]
    pub offered: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub saved: BTreeMap<String, Saved>,
    #[serde(default)]
    pub previous: Option<String>,
    /// Completed requests by receipt, kept with the values they changed so
    /// both publish in one rename.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub receipts: BTreeMap<String, String>,
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn engine(word: &str) -> Result<&'static str, String> {
    ENGINES
        .into_iter()
        .find(|e| *e == word)
        .ok_or_else(|| format!("unknown engine {word}"))
}

fn model_ok(model: &str) -> bool {
    !model.is_empty()
        && model
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:[]-".contains(&b))
}

/// The levels each engine takes. Claude's are fixed; Codex's models define
/// their own, so any plain word passes and Codex says when a model lacks it.
fn effort_ok(engine: &str, effort: &str) -> bool {
    if engine == "claude" {
        ["low", "medium", "high", "xhigh", "max"].contains(&effort)
    } else {
        !effort.is_empty()
            && effort
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(&b))
    }
}

/// `ROLE` or `ROLE@OWNER/REPO`, the repo folded: scopes match
/// case-insensitively, and folding on write leaves no duplicate keys.
pub fn key(text: &str) -> Result<String, String> {
    let (name, repo) = text
        .split_once('@')
        .map_or((text, None), |(n, r)| (n, Some(r)));
    if name.is_empty()
        || name.contains(char::is_whitespace)
        || repo.is_some_and(|r| !burrow_github::repo_name(r))
    {
        return Err("expected ROLE or ROLE@OWNER/REPO".into());
    }
    Ok(match repo {
        Some(r) => format!("{name}@{}", r.to_ascii_lowercase()),
        None => name.into(),
    })
}

/// `ENGINE [over PCT ENGINE]`, checked and spaced singly.
pub fn spec(words: &[&str]) -> Result<String, String> {
    match words {
        [primary] => Ok(engine(primary)?.into()),
        [primary, "over", pct, other] => {
            let value: f64 = pct.parse().map_err(|_| "overflow percent isn't a number")?;
            if !(0.0..=100.0).contains(&value) {
                return Err("overflow percent must be 0 to 100".into());
            }
            Ok(format!(
                "{} over {pct} {}",
                engine(primary)?,
                engine(other)?
            ))
        }
        _ => Err("expected ENGINE [over PCT ENGINE]".into()),
    }
}

impl Saved {
    /// The roles as lines, as [`Engines::roles_text`] gives them.
    pub fn roles_text_for(&self) -> String {
        self.roles
            .iter()
            .map(|(k, v)| format!("{k} {v}\n"))
            .collect()
    }
}

impl Engines {
    pub fn set_role(&mut self, role: &str, words: &[&str]) -> Result<(), String> {
        let spec = spec(words)?;
        self.roles.insert(key(role)?, spec);
        Ok(())
    }

    pub fn clear_role(&mut self, role: &str) -> Result<(), String> {
        self.roles
            .remove(&key(role)?)
            .map(|_| ())
            .ok_or_else(|| format!("{role} has no engine set"))
    }

    pub fn offer(&mut self, on: &str, models: &[&str]) -> Result<(), String> {
        let on = engine(on)?;
        if let Some(bad) = models.iter().find(|m| !model_ok(m)) {
            return Err(format!("invalid model id {bad}"));
        }
        let list = self.offered.entry(on.into()).or_default();
        for m in models {
            if !list.iter().any(|l| l == m) {
                list.push((*m).into());
            }
        }
        Ok(())
    }

    /// `ROLE ENGINE MODEL|- [EFFORT]`: refused unless the engine offers the
    /// model and takes the effort.
    pub fn set_model(&mut self, args: &[&str]) -> Result<(), String> {
        let (role, on, model, effort) = match args {
            [role, on, model] => (*role, *on, *model, None),
            [role, on, model, effort] => (*role, *on, *model, Some(*effort)),
            _ => return Err("expected ROLE[@OWNER/REPO] ENGINE MODEL|- [EFFORT]".into()),
        };
        let on = engine(on)?;
        let model = (model != "-").then_some(model);
        if let Some(m) = model {
            let offers = self.offered.get(on).map(Vec::as_slice).unwrap_or_default();
            if !model_ok(m) || !offers.iter().any(|o| o == m) {
                return Err(format!(
                    "{on} offers no model {m}; offered: {}",
                    if offers.is_empty() {
                        "none".into()
                    } else {
                        offers.join(", ")
                    }
                ));
            }
        }
        if let Some(e) = effort.filter(|e| !effort_ok(on, e)) {
            return Err(format!("{on} takes no effort {e}"));
        }
        self.models.insert(
            format!("{} {on}", key(role)?),
            Tuning {
                model: model.map(str::to_owned),
                effort: effort.map(str::to_owned),
            },
        );
        Ok(())
    }

    pub fn clear_model(&mut self, role: &str, on: &str) -> Result<(), String> {
        self.models
            .remove(&format!("{} {}", key(role)?, engine(on)?))
            .map(|_| ())
            .ok_or_else(|| format!("{role} has no {on} tuning"))
    }

    fn current(&self) -> Saved {
        Saved {
            roles: self.roles.clone(),
            models: self.models.clone(),
        }
    }

    /// The saved set equal to what is in force, if any.
    pub fn active(&self) -> Option<String> {
        let now = self.current();
        self.saved
            .iter()
            .find(|(_, s)| **s == now)
            .map(|(n, _)| n.clone())
    }

    /// The `engines` verbs: `save NAME [--replace]`, `use NAME|back`, `list`.
    pub fn apply(&mut self, args: &[&str], now: i64) -> Result<String, String> {
        let before = self.active();
        match args {
            ["list"] => {}
            ["save", name] | ["save", name, "--replace"] => {
                if !valid_name(name) || *name == "back" {
                    return Err("setup name must be [a-z0-9-]+ and not back".into());
                }
                if args.len() == 2 && self.saved.contains_key(*name) {
                    return Err(format!("setup {name} exists; save {name} --replace"));
                }
                self.saved.insert((*name).into(), self.current());
            }
            ["use", name] => {
                let name = if *name == "back" {
                    self.previous.clone().ok_or("no previous setup")?
                } else {
                    (*name).to_owned()
                };
                let target = self
                    .saved
                    .get(&name)
                    .cloned()
                    .ok_or_else(|| format!("unknown setup: {name}"))?;
                self.check(&target)
                    .map_err(|e| format!("setup {name}: {e}"))?;
                if before.as_deref() != Some(&name) {
                    let leaving = match &before {
                        Some(b) => b.clone(),
                        None => {
                            // Keep what is being replaced, so back returns to it.
                            let mut kept = format!("unsaved-{now}");
                            let mut n = 1;
                            while self.saved.contains_key(&kept) || kept == name {
                                n += 1;
                                kept = format!("unsaved-{now}-{n}");
                            }
                            self.saved.insert(kept.clone(), self.current());
                            kept
                        }
                    };
                    self.previous = Some(leaving);
                    self.roles = target.roles;
                    self.models = target.models;
                }
            }
            _ => {
                return Err(
                    "usage: engines [list | save NAME [--replace] | use NAME | use back]".into(),
                );
            }
        }
        Ok(self.status(before.as_deref()))
    }

    /// A saved set held to what the door holds a live one to, so a hand edit
    /// cannot put an invalid value in force.
    fn check(&self, set: &Saved) -> Result<(), String> {
        for (k, v) in &set.roles {
            key(k)?;
            spec(&v.split_whitespace().collect::<Vec<_>>())?;
        }
        for (k, t) in &set.models {
            let (role, on) = k.split_once(' ').ok_or("bad models key")?;
            key(role)?;
            let on = engine(on)?;
            if let Some(m) = &t.model
                && !self.offered.get(on).is_some_and(|o| o.contains(m))
            {
                return Err(format!("{on} offers no model {m}"));
            }
            if let Some(e) = t.effort.as_deref().filter(|e| !effort_ok(on, e)) {
                return Err(format!("{on} takes no effort {e}"));
            }
        }
        Ok(())
    }

    pub fn status(&self, before: Option<&str>) -> String {
        let active = self.active();
        let mut out = format!(
            "Was active: {}\nActive: {}\nPrevious: {}\n",
            before.unwrap_or("unsaved"),
            active.as_deref().unwrap_or("unsaved"),
            self.previous.as_deref().unwrap_or("none")
        );
        for name in self.saved.keys() {
            let mark = if active.as_deref() == Some(name) {
                "*"
            } else {
                " "
            };
            out += &format!("{mark} {name}\n");
        }
        out += &self.roles_text();
        out
    }

    /// The roles as lines, one `ROLE[@OWNER/REPO] ENGINE [over PCT ENGINE]` each.
    pub fn roles_text(&self) -> String {
        self.roles
            .iter()
            .map(|(k, v)| format!("{k} {v}\n"))
            .collect()
    }

    /// The models as lines, one `ROLE[@OWNER/REPO] ENGINE MODEL|- [EFFORT]` each.
    pub fn models_text(&self) -> String {
        self.models
            .iter()
            .map(|(k, t)| {
                let effort = t
                    .effort
                    .as_deref()
                    .map(|e| format!(" {e}"))
                    .unwrap_or_default();
                format!("{k} {}{effort}\n", t.model.as_deref().unwrap_or("-"))
            })
            .collect()
    }

    /// Whether anything is set: an untouched overlay shows the defaults.
    pub fn is_empty(&self) -> bool {
        self.roles.is_empty()
            && self.models.is_empty()
            && self.offered.is_empty()
            && self.saved.is_empty()
    }
}

/// The files the overlay replaced, read once into it. Each line is checked as
/// the door checks one; a model the old file named is offered along the way.
/// Not a reader: nothing else parses these formats.
pub fn import(
    roles: &str,
    models: &str,
    setups: &[(String, String)],
    previous: Option<String>,
) -> Result<Engines, String> {
    fn lines(text: &str) -> impl Iterator<Item = (usize, &str)> {
        text.lines()
            .enumerate()
            .map(|(i, l)| (i + 1, l.trim()))
            .filter(|(_, l)| !l.is_empty() && !l.starts_with('#'))
    }
    fn roles_of(text: &str, e: &mut Engines) -> Result<(), String> {
        for (n, line) in lines(text) {
            let words: Vec<_> = line.split_whitespace().collect();
            let (role, rest) = words.split_first().ok_or("empty")?;
            let at = |e: String| format!("roles line {n}: {line}: {e}");
            let k = key(role).map_err(at)?;
            if e.roles.contains_key(&k) {
                return Err(format!("roles line {n}: {line}: duplicate role key"));
            }
            e.set_role(role, rest)
                .map_err(|e| format!("roles line {n}: {line}: {e}"))?;
        }
        Ok(())
    }
    let mut out = Engines::default();
    roles_of(roles, &mut out)?;
    for (n, line) in lines(models) {
        let words: Vec<_> = line.split_whitespace().collect();
        if let [_, on, model, ..] = words.as_slice()
            && *model != "-"
        {
            out.offer(on, &[model])
                .map_err(|e| format!("models line {n}: {line}: {e}"))?;
        }
        let dup = words
            .get(..2)
            .and_then(|w| Some(format!("{} {}", key(w[0]).ok()?, w[1])));
        if dup.is_some_and(|d| out.models.contains_key(&d)) {
            return Err(format!("models line {n}: {line}: duplicate models key"));
        }
        out.set_model(&words)
            .map_err(|e| format!("models line {n}: {line}: {e}"))?;
    }
    for (name, text) in setups {
        if !valid_name(name) {
            continue;
        }
        let mut one = Engines::default();
        roles_of(text, &mut one).map_err(|e| format!("setup {name}: {e}"))?;
        out.saved.insert(
            name.clone(),
            Saved {
                roles: one.roles,
                models: out.models.clone(),
            },
        );
    }
    out.previous = previous.filter(|p| out.saved.contains_key(p));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use crate::Config;

    fn door(dir: &std::path::Path) -> Config {
        Config {
            overlay: dir.join("overlay.json"),
            legacy: Some(dir.join("proj")),
            ..Default::default()
        }
    }
    fn say(cfg: &Config, line: &str) -> Result<String, String> {
        cfg.command(
            &line
                .split_whitespace()
                .map(String::from)
                .collect::<Vec<_>>(),
            "test",
        )
    }

    #[test]
    fn the_door_refuses_a_model_or_effort_the_engine_does_not_offer() {
        let d = tempfile::tempdir().unwrap();
        let cfg = door(d.path());
        let err = say(&cfg, "model fix claude small").unwrap_err();
        assert!(err.contains("claude offers no model small"), "{err}");
        say(&cfg, "offer claude small big").unwrap();
        // Offered on Claude is not offered on Codex.
        assert!(say(&cfg, "model fix codex small").is_err());
        assert!(say(&cfg, "model fix claude huge").is_err());
        assert!(say(&cfg, "model fix claude small enormous").is_err());
        assert!(say(&cfg, "model fix claude small Max").is_err());
        say(&cfg, "model fix claude small max").unwrap();
        say(&cfg, "model fix codex - anything-plain").unwrap();
        assert_eq!(
            cfg.engines().unwrap().models["fix claude"].model.as_deref(),
            Some("small")
        );
        // Every change is logged, refusals are not.
        let log = std::fs::read_to_string(cfg.overlay.with_extension("log")).unwrap();
        assert_eq!(log.lines().count(), 3, "{log}");
    }

    #[test]
    fn a_bad_role_or_key_is_refused_and_a_repo_scope_is_folded() {
        let d = tempfile::tempdir().unwrap();
        let cfg = door(d.path());
        for bad in [
            "role fix old-model",
            "role fix codex:chosen",
            "role fix codex extra",
            "role fix claude over 101 codex",
            "role fix claude over nope codex",
            "role fix claude over 50",
            "role work@ codex",
            "role work@owner codex",
            "role work@owner/repo/extra codex",
            "role @owner/repo codex",
        ] {
            assert!(say(&cfg, bad).is_err(), "{bad}");
        }
        say(&cfg, "role work@Sam/Code codex").unwrap();
        say(&cfg, "role work@sam/code claude").unwrap();
        assert_eq!(
            cfg.engines().unwrap().roles_text(),
            "work@sam/code claude\n"
        );
    }

    #[test]
    fn engines_use_changes_exactly_the_saved_values_and_back_returns() {
        let d = tempfile::tempdir().unwrap();
        let cfg = door(d.path());
        say(&cfg, "offer claude small").unwrap();
        say(&cfg, "role fix codex").unwrap();
        say(&cfg, "engines save cheap").unwrap();
        say(&cfg, "role fix claude over 80 codex").unwrap();
        say(&cfg, "role judge claude").unwrap();
        say(&cfg, "model fix claude small low").unwrap();
        let usual = cfg.engines().unwrap();
        say(&cfg, "engines save usual").unwrap();
        assert!(say(&cfg, "engines save usual").is_err());
        let said = say(&cfg, "engines use cheap").unwrap();
        assert!(
            said.contains("Was active: usual\nActive: cheap\nPrevious: usual"),
            "{said}"
        );
        let cheap = cfg.engines().unwrap();
        assert_eq!(cheap.roles_text(), "fix codex\n");
        assert!(cheap.models.is_empty());
        // Nothing else moved.
        assert_eq!(cheap.offered, usual.offered);
        assert_eq!(cheap.saved.len(), 2);
        say(&cfg, "engines use back").unwrap();
        let back = cfg.engines().unwrap();
        assert_eq!((back.roles, back.models), (usual.roles, usual.models));
        assert!(say(&cfg, "engines use absent").is_err());
        // An unsaved state is kept before it is replaced, so back returns to it.
        say(&cfg, "role extra codex").unwrap();
        say(&cfg, "engines use cheap").unwrap();
        say(&cfg, "engines use back").unwrap();
        assert!(cfg.engines().unwrap().roles.contains_key("extra"));
    }

    #[test]
    fn the_old_files_migrate_once() {
        let d = tempfile::tempdir().unwrap();
        let cfg = door(d.path());
        let old = d.path().join("proj");
        assert!(!cfg.migrate().unwrap(), "no old files, nothing to do");
        std::fs::create_dir_all(old.join("roles.d")).unwrap();
        std::fs::write(
            old.join("roles.d/usual"),
            "# usual\nfix@Sam/Box codex over 80 claude\nwork claude\n",
        )
        .unwrap();
        std::fs::write(old.join("roles.d/cheap"), "work codex\n").unwrap();
        std::fs::write(old.join("roles.previous"), "cheap\n").unwrap();
        std::os::unix::fs::symlink(old.join("roles.d/usual"), old.join("roles")).unwrap();
        std::fs::write(
            old.join("models"),
            "work claude small low\nfix codex - high\n",
        )
        .unwrap();
        // An old file that doesn't parse stops it with everything left in place.
        std::fs::write(old.join("models"), "work claude small enormous\n").unwrap();
        let err = cfg.migrate().unwrap_err();
        assert!(err.contains("claude takes no effort enormous"), "{err}");
        assert!(
            old.join("roles").symlink_metadata().is_ok()
                && !cfg.overlay.with_file_name("engines.json").exists()
        );
        std::fs::write(
            old.join("models"),
            "work claude small low\nfix codex - high\n",
        )
        .unwrap();
        assert!(cfg.migrate().unwrap());
        let e = cfg.engines().unwrap();
        assert_eq!(
            e.roles_text(),
            "fix@sam/box codex over 80 claude\nwork claude\n"
        );
        assert_eq!(e.models_text(), "fix codex - high\nwork claude small low\n");
        assert_eq!(e.offered["claude"], ["small"]);
        assert_eq!(e.active().as_deref(), Some("usual"));
        assert_eq!(e.previous.as_deref(), Some("cheap"));
        assert_eq!(e.saved["cheap"].roles_text_for(), "work codex\n");
        // The old files stay for a rollback; a second call is a no-op.
        assert!(old.join("roles").symlink_metadata().is_ok());
        let engines = cfg.overlay.with_file_name("engines.json");
        let after = std::fs::read(&engines).unwrap();
        assert!(!cfg.migrate().unwrap());
        assert_eq!(std::fs::read(&engines).unwrap(), after);
        // The overlay a previous generation reads is untouched by any of it.
        assert!(!cfg.overlay.exists());
        let log = std::fs::read_to_string(cfg.overlay.with_extension("log")).unwrap();
        assert!(log.contains("\"actor\":\"migration\""), "{log}");
        // A link to nowhere is a broken setting, not an empty one, and nothing is written.
        let d = tempfile::tempdir().unwrap();
        let cfg = door(d.path());
        std::fs::create_dir_all(d.path().join("proj")).unwrap();
        std::os::unix::fs::symlink("/nowhere", d.path().join("proj/roles")).unwrap();
        assert!(cfg.migrate().is_err());
        assert!(!cfg.overlay.with_file_name("engines.json").exists());
    }

    #[test]
    fn a_kept_unsaved_setup_never_overwrites_another_and_a_bad_saved_one_is_not_activated() {
        let d = tempfile::tempdir().unwrap();
        let cfg = door(d.path());
        say(&cfg, "role a claude").unwrap();
        say(&cfg, "engines save one").unwrap();
        say(&cfg, "role a codex").unwrap();
        say(&cfg, "engines save two").unwrap();
        say(&cfg, "role b codex").unwrap();
        say(&cfg, "engines use one").unwrap();
        say(&cfg, "role c codex").unwrap();
        say(&cfg, "engines use two").unwrap();
        let kept = cfg
            .engines()
            .unwrap()
            .saved
            .keys()
            .filter(|n| n.starts_with("unsaved-"))
            .count();
        assert_eq!(kept, 2);
        // A hand-edited saved set that the door would refuse is not put in force.
        let path = cfg.overlay.with_file_name("engines.json");
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(
            &path,
            text.replace("\"a\":\"claude\"", "\"a\":\"old-model\""),
        )
        .unwrap();
        let err = say(&cfg, "engines use one").unwrap_err();
        assert!(err.contains("setup one"), "{err}");
    }
}
